//! Wayland backend: layer-shell overlay, virtual pointer, and keyboard input.
//!
//! Manages the Wayland connection, creates a full-screen layer-shell surface for
//! the overlay, and translates raw keyboard events into semantic [`KeyEvent`]s.

use std::collections::VecDeque;
use std::io::Write;
use std::os::fd::AsFd;
use std::time::{SystemTime, UNIX_EPOCH};

use wayland_client::protocol::wl_pointer::{Axis, AxisSource, ButtonState};
use wayland_client::{
    delegate_noop,
    protocol::{
        wl_buffer, wl_compositor, wl_keyboard, wl_pointer, wl_region, wl_registry, wl_seat, wl_shm,
        wl_shm_pool, wl_surface,
    },
    Connection, Dispatch, EventQueue, QueueHandle, WEnum,
};
use wayland_protocols_wlr::{
    layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1},
    virtual_pointer::v1::client::{zwlr_virtual_pointer_manager_v1, zwlr_virtual_pointer_v1},
};

use anyhow::{Context, Result};

// Linux evdev button codes.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

// Linux evdev key codes for modifier keys.
const KEY_LCTRL: u32 = 29;
const KEY_LSHIFT: u32 = 42;
const KEY_LALT: u32 = 56;
const KEY_RSHIFT: u32 = 54;
const KEY_RCTRL: u32 = 97;
const KEY_RALT: u32 = 100;

// Modifier bitmasks in the Wayland `mods_depressed` field.
const MOD_SHIFT: u32 = 1;
const MOD_CTRL: u32 = 4;
const MOD_ALT: u32 = 8;

// Drag animation timing (~150ms total).
const DRAG_SETTLE_MS: u64 = 25;
const DRAG_PRESS_SETTLE_MS: u64 = 30;
const DRAG_INTERP_STEPS: u32 = 12;
const DRAG_STEP_DELAY_MS: u64 = 5;
const DRAG_RELEASE_SETTLE_MS: u64 = 35;

// Maximum number of event loop iterations to wait for a key release before
// giving up.  Prevents an infinite hang if the compositor drops the event.
const KEY_RELEASE_MAX_DISPATCHES: u32 = 200;

// ===========================================================================
// Public key event type.
// ===========================================================================

/// Semantic key event emitted by the backend and consumed by the main loop.
pub enum KeyEvent {
    /// A printable character (post-shift).
    Char(char),
    /// Ctrl held while pressing a character key.
    CtrlChar(char),
    /// Alt held while pressing a character key.
    AltChar(char),
    /// Space — mapped to left-click.
    Click,
    /// Enter — mapped to double-click.
    DoubleClick,
    /// Shift+Enter — mapped to triple-click.
    TripleClick,
    /// Period — mapped to right-click.
    RightClick,
    /// Escape or Ctrl-[ — close the overlay.
    Close,
    /// Backspace — undo one selection step.
    Undo,
    ScrollUp,
    ScrollDown,
    ScrollLeft,
    ScrollRight,
}

// ===========================================================================
// Internal physical key abstraction.
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PhysicalKey {
    Char(char),
    Space,
    Enter,
    Escape,
    Backspace,
    Up,
    Down,
    Left,
    Right,
}

/// Maps a physical key to the semantic `KeyEvent` the main loop cares about.
fn physical_to_event(key: PhysicalKey) -> Option<KeyEvent> {
    match key {
        PhysicalKey::Space => Some(KeyEvent::Click),
        PhysicalKey::Enter => Some(KeyEvent::DoubleClick),
        PhysicalKey::Escape => Some(KeyEvent::Close),
        PhysicalKey::Backspace => Some(KeyEvent::Undo),
        PhysicalKey::Up => Some(KeyEvent::ScrollUp),
        PhysicalKey::Down => Some(KeyEvent::ScrollDown),
        PhysicalKey::Left => Some(KeyEvent::ScrollLeft),
        PhysicalKey::Right => Some(KeyEvent::ScrollRight),
        PhysicalKey::Char('.') => Some(KeyEvent::RightClick),
        PhysicalKey::Char(c) => Some(KeyEvent::Char(c)),
    }
}

// ===========================================================================
// WaylandBackend — public API for the main loop.
// ===========================================================================

/// Owns the Wayland connection, event queue, and all protocol objects.
///
/// Designed so that public methods can freely call `roundtrip` /
/// `blocking_dispatch` without borrowing conflicts.
pub struct WaylandBackend {
    state: WaylandState,
    event_queue: EventQueue<WaylandState>,
    queue_handle: QueueHandle<WaylandState>,
}

impl WaylandBackend {
    pub fn new() -> Result<Self> {
        let conn = Connection::connect_to_env().context("connect to Wayland display")?;
        let mut event_queue = conn.new_event_queue();
        let queue_handle = event_queue.handle();

        conn.display().get_registry(&queue_handle, ());

        let mut state = WaylandState {
            conn,
            compositor: None,
            shm: None,
            surface: None,
            layer_shell: None,
            layer_surface: None,
            seat: None,
            vp_manager: None,
            virtual_pointer: None,
            screen_w: 0,
            screen_h: 0,
            configured: false,
            pending_keys: VecDeque::new(),
            shift_held: false,
            ctrl_held: false,
            alt_held: false,
            awaiting_key_release: false,
        };

        event_queue
            .roundtrip(&mut state)
            .context("initial roundtrip")?;

        // Verify required protocol support before proceeding.
        if state.compositor.is_none() {
            anyhow::bail!("compositor does not advertise wl_compositor");
        }
        if state.shm.is_none() {
            anyhow::bail!("compositor does not advertise wl_shm");
        }
        if state.layer_shell.is_none() {
            anyhow::bail!(
                "compositor does not support wlr-layer-shell-unstable-v1. \
                 Supported compositors: Sway, Hyprland, river, etc."
            );
        }
        if state.vp_manager.is_none() {
            anyhow::bail!(
                "compositor does not support wlr-virtual-pointer-unstable-v1. \
                 Supported compositors: Sway, Hyprland, river, etc."
            );
        }

        // Bind the virtual pointer now that we know which globals are available.
        if let Some(manager) = state.vp_manager.take() {
            state.virtual_pointer =
                Some(manager.create_virtual_pointer(state.seat.as_ref(), &queue_handle, ()));
        }

        state.init_layer_surface(&queue_handle)?;
        event_queue
            .roundtrip(&mut state)
            .context("roundtrip after layer surface")?;

        while !state.configured {
            event_queue
                .blocking_dispatch(&mut state)
                .context("waiting for configure")?;
        }

        Ok(Self {
            state,
            event_queue,
            queue_handle,
        })
    }

    // -- Queries ------------------------------------------------------------

    pub fn screen_size(&self) -> (u32, u32) {
        (self.state.screen_w, self.state.screen_h)
    }

    // -- Event loop ---------------------------------------------------------

    /// Blocks until the next key event arrives, or returns `None` when the
    /// surface has been torn down (i.e. the overlay is closed).
    pub fn next_key(&mut self) -> Result<Option<KeyEvent>> {
        loop {
            if let Some(key) = self.state.pending_keys.pop_front() {
                return Ok(Some(key));
            }
            if self.state.surface.is_none() {
                return Ok(None);
            }
            self.event_queue
                .blocking_dispatch(&mut self.state)
                .context("blocking_dispatch")?;
        }
    }

    // -- Rendering ----------------------------------------------------------

    /// Copy an ARGB8888 pixel buffer to the overlay surface via wl_shm.
    pub fn present(&mut self, pixels: &[u8], width: u32, height: u32) -> Result<()> {
        // Check availability before allocating resources.
        let shm = self.state.shm.as_ref().context("wl_shm not available")?;
        let surface = self
            .state
            .surface
            .as_ref()
            .context("wl_surface not available")?;

        let stride = width * 4;
        let mut file = tempfile::tempfile().context("create shm tempfile")?;
        file.write_all(pixels).context("write pixel buffer")?;

        let pool = shm.create_pool(file.as_fd(), pixels.len() as i32, &self.queue_handle, ());
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride as i32,
            wl_shm::Format::Argb8888,
            &self.queue_handle,
            (),
        );

        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, width as i32, height as i32);
        surface.commit();
        pool.destroy();
        Ok(())
    }

    // -- Mouse actions ------------------------------------------------------

    /// Warp the virtual pointer to absolute screen coordinates.
    pub fn move_mouse(&mut self, x: u32, y: u32) -> Result<()> {
        if let Some(vp) = &self.state.virtual_pointer {
            vp.motion_absolute(timestamp(), x, y, self.state.screen_w, self.state.screen_h);
            vp.frame();
        }
        self.state.conn.flush().context("flush after move_mouse")?;
        Ok(())
    }

    /// Tear down the overlay, move to `(x, y)`, and left-click once.
    pub fn click(&mut self, x: u32, y: u32) -> Result<()> {
        self.click_at(x, y, BTN_LEFT, 1)
    }

    /// Tear down the overlay, move to `(x, y)`, and left-click twice.
    pub fn double_click(&mut self, x: u32, y: u32) -> Result<()> {
        self.click_at(x, y, BTN_LEFT, 2)
    }

    /// Tear down the overlay, move to `(x, y)`, and left-click three times.
    pub fn triple_click(&mut self, x: u32, y: u32) -> Result<()> {
        self.click_at(x, y, BTN_LEFT, 3)
    }

    /// Tear down the overlay, move to `(x, y)`, and right-click once.
    pub fn right_click(&mut self, x: u32, y: u32) -> Result<()> {
        self.click_at(x, y, BTN_RIGHT, 1)
    }

    /// Tear down the overlay, then perform a click-drag from `(x1, y1)` to
    /// `(x2, y2)`.
    ///
    /// Simulates a realistic drag by sending intermediate motion events
    /// over time so that compositors and applications recognize the gesture.
    pub fn drag_select(&mut self, x1: u32, y1: u32, x2: u32, y2: u32) -> Result<()> {
        use std::thread::sleep;
        use std::time::Duration;

        self.teardown_surface()?;
        let (sw, sh) = (self.state.screen_w, self.state.screen_h);

        // 1. Move to the drag origin and let focus settle.
        self.send_motion(x1, y1, sw, sh)?;
        self.roundtrip("motion to drag start")?;
        sleep(Duration::from_millis(DRAG_SETTLE_MS));

        // 2. Press and hold — give the app time to register the press.
        self.send_button(BTN_LEFT, ButtonState::Pressed)?;
        self.roundtrip("press at drag start")?;
        sleep(Duration::from_millis(DRAG_PRESS_SETTLE_MS));

        // 3. Animate the pointer from start to end so apps see continuous
        //    motion and cross their drag threshold.
        let step_delay = Duration::from_millis(DRAG_STEP_DELAY_MS);
        for i in 1..=DRAG_INTERP_STEPS {
            let t = i as f64 / DRAG_INTERP_STEPS as f64;
            let x = x1 as f64 + (x2 as f64 - x1 as f64) * t;
            let y = y1 as f64 + (y2 as f64 - y1 as f64) * t;
            let cx = (x.clamp(0.0, sw.saturating_sub(1) as f64)) as u32;
            let cy = (y.clamp(0.0, sh.saturating_sub(1) as f64)) as u32;
            self.send_motion(cx, cy, sw, sh)?;
            self.state
                .conn
                .flush()
                .context("flush during drag motion")?;
            sleep(step_delay);
        }
        self.roundtrip("motion to drag end")?;
        sleep(Duration::from_millis(DRAG_RELEASE_SETTLE_MS));

        // 4. Release the button at the destination.
        self.send_button(BTN_LEFT, ButtonState::Released)?;
        self.roundtrip("release at drag end")
    }

    // -- Scroll -------------------------------------------------------------

    pub fn scroll_up(&mut self) -> Result<()> {
        self.scroll(Axis::VerticalScroll, -15.0, -1)
    }

    pub fn scroll_down(&mut self) -> Result<()> {
        self.scroll(Axis::VerticalScroll, 15.0, 1)
    }

    pub fn scroll_left(&mut self) -> Result<()> {
        self.scroll(Axis::HorizontalScroll, -15.0, -1)
    }

    pub fn scroll_right(&mut self) -> Result<()> {
        self.scroll(Axis::HorizontalScroll, 15.0, 1)
    }

    // -- Lifecycle ----------------------------------------------------------

    /// Block until the key that triggered the current action is released.
    /// This prevents the key-up event from leaking to the underlying window
    /// after the overlay is torn down.
    pub fn wait_for_key_release(&mut self) -> Result<()> {
        self.state.awaiting_key_release = true;
        let mut dispatches = 0u32;
        while self.state.awaiting_key_release {
            self.event_queue
                .blocking_dispatch(&mut self.state)
                .context("waiting for key release")?;
            dispatches += 1;
            if dispatches >= KEY_RELEASE_MAX_DISPATCHES {
                self.state.awaiting_key_release = false;
                break;
            }
        }
        Ok(())
    }

    /// Destroy the virtual pointer and tear down the overlay surface.
    ///
    /// Does NOT send safety button releases — the compositor releases all
    /// buttons when the virtual pointer object is destroyed. Sending
    /// redundant releases here caused spurious extra clicks (e.g. double-click
    /// turning into triple-click).
    pub fn exit(&mut self) -> Result<()> {
        if let Some(vp) = self.state.virtual_pointer.take() {
            vp.destroy();
        }
        self.state.conn.flush().context("flush vp destroy")?;

        self.teardown_surface()
    }

    /// Re-create the overlay after it was torn down (e.g. for scroll).
    pub fn reopen(&mut self) -> Result<()> {
        self.state.configured = false;
        self.state.init_layer_surface(&self.queue_handle)?;
        self.roundtrip("reopen")?;
        while !self.state.configured {
            self.event_queue
                .blocking_dispatch(&mut self.state)
                .context("waiting for configure after reopen")?;
        }
        Ok(())
    }

    // -- Private helpers ----------------------------------------------------

    /// Shared implementation for click, double_click, and right_click.
    ///
    /// Tears down the overlay so the compositor updates focus, moves the
    /// virtual pointer to `(x, y)`, then sends `count` press/release pairs
    /// on `button`.
    fn click_at(&mut self, x: u32, y: u32, button: u32, count: u32) -> Result<()> {
        self.teardown_surface()?;

        self.send_motion(x, y, self.state.screen_w, self.state.screen_h)?;
        self.roundtrip("motion before click")?;

        if let Some(vp) = &self.state.virtual_pointer {
            for _ in 0..count {
                let ts = timestamp();
                vp.button(ts, button, ButtonState::Pressed);
                vp.frame();
                vp.button(ts, button, ButtonState::Released);
                vp.frame();
            }
        }
        self.roundtrip("click")
    }

    fn scroll(&mut self, axis: Axis, value: f64, discrete: i32) -> Result<()> {
        self.teardown_surface()?;

        if let Some(vp) = &self.state.virtual_pointer {
            // axis_source tells the compositor this is a wheel event, not a
            // touchpad gesture. axis_discrete provides the notch count that
            // many compositors require alongside the continuous value.
            vp.axis_source(AxisSource::Wheel);
            vp.axis_discrete(timestamp(), axis, value, discrete);
            vp.frame();
        }
        self.state.conn.flush().context("flush after scroll")?;

        self.reopen()
    }

    fn teardown_surface(&mut self) -> Result<()> {
        if let Some(layer_surface) = self.state.layer_surface.take() {
            layer_surface.destroy();
        }
        if let Some(surface) = self.state.surface.take() {
            surface.destroy();
        }
        self.roundtrip("surface teardown")
    }

    /// Convenience wrapper around `event_queue.roundtrip` with context.
    fn roundtrip(&mut self, context: &str) -> Result<()> {
        self.event_queue
            .roundtrip(&mut self.state)
            .with_context(|| format!("roundtrip ({context})"))?;
        Ok(())
    }

    fn send_motion(&self, x: u32, y: u32, sw: u32, sh: u32) -> Result<()> {
        if let Some(vp) = &self.state.virtual_pointer {
            vp.motion_absolute(timestamp(), x, y, sw, sh);
            vp.frame();
        }
        Ok(())
    }

    fn send_button(&self, button: u32, button_state: ButtonState) -> Result<()> {
        if let Some(vp) = &self.state.virtual_pointer {
            vp.button(timestamp(), button, button_state);
            vp.frame();
        }
        Ok(())
    }
}

impl Drop for WaylandBackend {
    fn drop(&mut self) {
        let _ = self.exit();
    }
}

// ===========================================================================
// Internal Wayland dispatch state.
// ===========================================================================

struct WaylandState {
    conn: Connection,

    // Globals bound during registry enumeration.
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    seat: Option<wl_seat::WlSeat>,
    layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    vp_manager: Option<zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1>,

    // Per-session objects.
    surface: Option<wl_surface::WlSurface>,
    layer_surface: Option<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1>,
    virtual_pointer: Option<zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1>,

    // Screen geometry reported by the layer-surface configure.
    screen_w: u32,
    screen_h: u32,
    configured: bool,

    // Keyboard state.
    pending_keys: VecDeque<KeyEvent>,
    shift_held: bool,
    ctrl_held: bool,
    alt_held: bool,
    awaiting_key_release: bool,
}

impl WaylandState {
    /// Create and configure a full-screen layer-shell overlay surface.
    fn init_layer_surface(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        let compositor = self
            .compositor
            .as_ref()
            .context("wl_compositor not available")?;
        let layer_shell = self
            .layer_shell
            .as_ref()
            .context("zwlr_layer_shell_v1 not available")?;

        let surface = compositor.create_surface(qh, ());
        let layer_surface = layer_shell.get_layer_surface(
            &surface,
            None,
            zwlr_layer_shell_v1::Layer::Overlay,
            "mousefree".to_string(),
            qh,
            (),
        );

        layer_surface.set_size(0, 0);
        layer_surface.set_anchor(
            zwlr_layer_surface_v1::Anchor::Top
                | zwlr_layer_surface_v1::Anchor::Bottom
                | zwlr_layer_surface_v1::Anchor::Left
                | zwlr_layer_surface_v1::Anchor::Right,
        );
        layer_surface.set_exclusive_zone(-1);
        layer_surface
            .set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::Exclusive);

        surface.commit();

        self.surface = Some(surface);
        self.layer_surface = Some(layer_surface);
        Ok(())
    }
}

// ===========================================================================
// Wayland dispatch implementations.
// ===========================================================================

impl Dispatch<wl_registry::WlRegistry, ()> for WaylandState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "wl_compositor" => {
                state.compositor = Some(registry.bind(name, version.min(4), qh, ()));
            }
            "wl_shm" => {
                state.shm = Some(registry.bind(name, 1, qh, ()));
            }
            "wl_seat" => {
                state.seat = Some(registry.bind(name, version.min(7), qh, ()));
            }
            "zwlr_layer_shell_v1" => {
                state.layer_shell = Some(registry.bind(name, version.min(4), qh, ()));
            }
            "zwlr_virtual_pointer_manager_v1" => {
                state.vp_manager = Some(registry.bind(name, version.min(2), qh, ()));
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
        {
            if caps.contains(wl_seat::Capability::Keyboard) {
                seat.get_keyboard(qh, ());
            }
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for WaylandState {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Modifiers { mods_depressed, .. } => {
                state.shift_held = (mods_depressed & MOD_SHIFT) != 0;
                state.ctrl_held = (mods_depressed & MOD_CTRL) != 0;
                state.alt_held = (mods_depressed & MOD_ALT) != 0;
            }
            wl_keyboard::Event::Key {
                key,
                state: WEnum::Value(wl_keyboard::KeyState::Pressed),
                ..
            } => {
                // Ignore bare modifier press events; we track modifiers via
                // the Modifiers event instead.
                if matches!(key, KEY_LSHIFT | KEY_RSHIFT | KEY_LCTRL | KEY_RCTRL | KEY_LALT | KEY_RALT) {
                    return;
                }
                // Ctrl-[ acts as Escape (evdev keycode 26 = '[').
                if key == 26 && state.ctrl_held {
                    state.pending_keys.push_back(KeyEvent::Close);
                    return;
                }
                if let Some(physical) = keycode_to_key(key, state.shift_held) {
                    // Shift+Enter → triple-click.
                    if physical == PhysicalKey::Enter && state.shift_held {
                        state.pending_keys.push_back(KeyEvent::TripleClick);
                        return;
                    }
                    // Modifier+letter emits CtrlChar/AltChar so the main loop
                    // can distinguish nudge tiers.
                    if let PhysicalKey::Char(c) = physical {
                        if state.ctrl_held {
                            state.pending_keys.push_back(KeyEvent::CtrlChar(c));
                            return;
                        }
                        if state.alt_held {
                            state.pending_keys.push_back(KeyEvent::AltChar(c));
                            return;
                        }
                    }
                    if let Some(ev) = physical_to_event(physical) {
                        state.pending_keys.push_back(ev);
                    }
                }
            }
            wl_keyboard::Event::Key {
                state: WEnum::Value(wl_keyboard::KeyState::Released),
                ..
            } => {
                state.awaiting_key_release = false;
            }
            _ => {}
        }
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for WaylandState {
    fn event(
        state: &mut Self,
        layer_surface: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                layer_surface.ack_configure(serial);
                const MAX_SCREEN_DIM: u32 = 16384;
                if width > 0 && height > 0 && width <= MAX_SCREEN_DIM && height <= MAX_SCREEN_DIM {
                    state.screen_w = width;
                    state.screen_h = height;
                    state.configured = true;
                }
            }
            zwlr_layer_surface_v1::Event::Closed => {
                state.pending_keys.push_back(KeyEvent::Close);
            }
            _ => {}
        }
    }
}

delegate_noop!(WaylandState: ignore wl_compositor::WlCompositor);
delegate_noop!(WaylandState: ignore wl_region::WlRegion);
delegate_noop!(WaylandState: ignore wl_surface::WlSurface);
delegate_noop!(WaylandState: ignore wl_shm::WlShm);
delegate_noop!(WaylandState: ignore wl_shm_pool::WlShmPool);
delegate_noop!(WaylandState: ignore wl_buffer::WlBuffer);
delegate_noop!(WaylandState: ignore wl_pointer::WlPointer);
delegate_noop!(WaylandState: ignore zwlr_layer_shell_v1::ZwlrLayerShellV1);
delegate_noop!(WaylandState: ignore zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1);
delegate_noop!(WaylandState: ignore zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1);

// ===========================================================================
// Helpers.
// ===========================================================================

/// Returns a u32 millisecond timestamp for Wayland pointer events.
///
/// Wayland timestamps are u32 milliseconds from an arbitrary epoch.
/// Wrapping every ~49 days is expected and harmless per the protocol.
fn timestamp() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u32
}

/// Translates a Linux evdev key code into a [`PhysicalKey`].
///
/// **Note:** This table assumes a US QWERTY layout. Other layouts will
/// produce incorrect characters. A proper fix requires xkb integration.
fn keycode_to_key(kc: u32, shift_held: bool) -> Option<PhysicalKey> {
    // Non-character keys — independent of shift state.
    match kc {
        1 => return Some(PhysicalKey::Escape),
        14 => return Some(PhysicalKey::Backspace),
        28 => return Some(PhysicalKey::Enter),
        57 => return Some(PhysicalKey::Space),
        104 => return Some(PhysicalKey::Up),
        105 => return Some(PhysicalKey::Left),
        106 => return Some(PhysicalKey::Right),
        109 => return Some(PhysicalKey::Down),
        _ => {}
    }

    // Character keys — shift selects the alternate mapping.
    let ch = if shift_held {
        match kc {
            2 => '!',
            3 => '@',
            4 => '#',
            5 => '$',
            6 => '%',
            7 => '^',
            8 => '&',
            9 => '*',
            10 => '(',
            11 => ')',
            12 => '_',
            13 => '+',
            26 => '{',
            27 => '}',
            43 => '|',
            39 => ':',
            40 => '"',
            41 => '~',
            51 => '<',
            52 => '>',
            53 => '?',
            16 => 'Q',
            17 => 'W',
            18 => 'E',
            19 => 'R',
            20 => 'T',
            21 => 'Y',
            22 => 'U',
            23 => 'I',
            24 => 'O',
            25 => 'P',
            30 => 'A',
            31 => 'S',
            32 => 'D',
            33 => 'F',
            34 => 'G',
            35 => 'H',
            36 => 'J',
            37 => 'K',
            38 => 'L',
            44 => 'Z',
            45 => 'X',
            46 => 'C',
            47 => 'V',
            48 => 'B',
            49 => 'N',
            50 => 'M',
            _ => return None,
        }
    } else {
        match kc {
            2 => '1',
            3 => '2',
            4 => '3',
            5 => '4',
            6 => '5',
            7 => '6',
            8 => '7',
            9 => '8',
            10 => '9',
            11 => '0',
            12 => '-',
            13 => '=',
            26 => '[',
            27 => ']',
            43 => '\\',
            39 => ';',
            40 => '\'',
            41 => '`',
            51 => ',',
            52 => '.',
            53 => '/',
            16 => 'q',
            17 => 'w',
            18 => 'e',
            19 => 'r',
            20 => 't',
            21 => 'y',
            22 => 'u',
            23 => 'i',
            24 => 'o',
            25 => 'p',
            30 => 'a',
            31 => 's',
            32 => 'd',
            33 => 'f',
            34 => 'g',
            35 => 'h',
            36 => 'j',
            37 => 'k',
            38 => 'l',
            44 => 'z',
            45 => 'x',
            46 => 'c',
            47 => 'v',
            48 => 'b',
            49 => 'n',
            50 => 'm',
            _ => return None,
        }
    };
    Some(PhysicalKey::Char(ch))
}

// ===========================================================================
// Tests.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keycode_letters_unshifted() {
        assert_eq!(keycode_to_key(16, false), Some(PhysicalKey::Char('q')));
        assert_eq!(keycode_to_key(30, false), Some(PhysicalKey::Char('a')));
        assert_eq!(keycode_to_key(44, false), Some(PhysicalKey::Char('z')));
        assert_eq!(keycode_to_key(50, false), Some(PhysicalKey::Char('m')));
    }

    #[test]
    fn keycode_letters_shifted() {
        assert_eq!(keycode_to_key(16, true), Some(PhysicalKey::Char('Q')));
        assert_eq!(keycode_to_key(30, true), Some(PhysicalKey::Char('A')));
        assert_eq!(keycode_to_key(50, true), Some(PhysicalKey::Char('M')));
    }

    #[test]
    fn keycode_digits() {
        assert_eq!(keycode_to_key(2, false), Some(PhysicalKey::Char('1')));
        assert_eq!(keycode_to_key(11, false), Some(PhysicalKey::Char('0')));
        assert_eq!(keycode_to_key(2, true), Some(PhysicalKey::Char('!')));
        assert_eq!(keycode_to_key(11, true), Some(PhysicalKey::Char(')')));
    }

    #[test]
    fn keycode_special_keys() {
        assert_eq!(keycode_to_key(1, false), Some(PhysicalKey::Escape));
        assert_eq!(keycode_to_key(14, false), Some(PhysicalKey::Backspace));
        assert_eq!(keycode_to_key(28, false), Some(PhysicalKey::Enter));
        assert_eq!(keycode_to_key(57, false), Some(PhysicalKey::Space));
        // Delete (111) is intentionally unmapped — returns None so the key is
        // consumed by the overlay without triggering any action.
        assert_eq!(keycode_to_key(111, false), None);
    }

    #[test]
    fn keycode_special_keys_ignore_shift() {
        assert_eq!(keycode_to_key(1, true), Some(PhysicalKey::Escape));
        assert_eq!(keycode_to_key(57, true), Some(PhysicalKey::Space));
    }

    #[test]
    fn keycode_unknown_returns_none() {
        assert_eq!(keycode_to_key(999, false), None);
        assert_eq!(keycode_to_key(999, true), None);
    }

    #[test]
    fn keycode_punctuation() {
        assert_eq!(keycode_to_key(52, false), Some(PhysicalKey::Char('.')));
        assert_eq!(keycode_to_key(53, false), Some(PhysicalKey::Char('/')));
        assert_eq!(keycode_to_key(52, true), Some(PhysicalKey::Char('>')));
        assert_eq!(keycode_to_key(53, true), Some(PhysicalKey::Char('?')));
    }

    #[test]
    fn physical_to_event_mappings() {
        assert!(matches!(
            physical_to_event(PhysicalKey::Space),
            Some(KeyEvent::Click)
        ));
        assert!(matches!(
            physical_to_event(PhysicalKey::Enter),
            Some(KeyEvent::DoubleClick)
        ));
        assert!(matches!(
            physical_to_event(PhysicalKey::Escape),
            Some(KeyEvent::Close)
        ));
        assert!(matches!(
            physical_to_event(PhysicalKey::Backspace),
            Some(KeyEvent::Undo)
        ));
        assert!(matches!(
            physical_to_event(PhysicalKey::Char('.')),
            Some(KeyEvent::RightClick)
        ));
        assert!(matches!(
            physical_to_event(PhysicalKey::Char('a')),
            Some(KeyEvent::Char('a'))
        ));
        // All PhysicalKey variants now map to a KeyEvent (no dead paths).
    }

    #[test]
    fn timestamp_does_not_panic() {
        // Mainly confirms unwrap_or_default works even if the clock is weird.
        let _ = timestamp();
    }
}
