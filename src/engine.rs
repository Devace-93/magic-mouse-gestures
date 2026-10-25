//! Gesture recognition for one Magic Mouse. Consumes raw evdev events from
//! the grabbed device, forwards pointer/button/wheel events to the virtual
//! mouse, and turns multi-finger touches into gestures.

use crate::config::{Action, Axis, Config, Direction, GestureKind, SwipeDir};
use crate::hidraw::RawTouch;
use crate::virt::{VirtualKeyboard, VirtualMouse, VirtualTouchpad};
use anyhow::Result;
use evdev::{AbsoluteAxisCode, EventType, InputEvent, RelativeAxisCode};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

const MAX_SLOTS: usize = 16;

/// Calibration of one multitouch axis of the real device.
#[derive(Debug, Clone, Copy)]
pub struct AxisInfo {
    pub min: i32,
    pub max: i32,
    /// Units per millimetre.
    pub res: f64,
}

impl AxisInfo {
    fn to_mm(&self, v: i32) -> f64 {
        (v - self.min) as f64 / self.res
    }
}

#[derive(Debug, Clone, Copy)]
struct Touch {
    /// Contact id (hidraw touch id, or evdev tracking id).
    id: i32,
    x: i32,
    y: i32,
    start_x: i32,
    start_y: i32,
    /// True until the first SYN_REPORT after the contact appeared; position
    /// events in that first frame still define the landing point.
    fresh: bool,
}

/// Devices shared by every mouse: one touchpad and one keyboard.
pub struct Shared {
    pub touchpad: VirtualTouchpad,
    pub keyboard: VirtualKeyboard,
}

#[derive(Debug)]
enum SwipeState {
    /// Fingers are down but have not travelled far enough yet.
    Armed,
    /// Replaying motion on the virtual touchpad; `sign` flips direction,
    /// `axis` picks the component forwarded.
    Workspace { sign: f64, axis: Axis },
    /// A discrete swipe rule already fired (or nothing matched); wait for the
    /// finger count to change.
    Done,
}

struct Session {
    start: Instant,
    max_fingers: usize,
    count: usize,
    /// Centroid (mm) when the current finger count was established.
    anchor: (f64, f64),
    last_centroid: (f64, f64),
    swipe: SwipeState,
    /// True once any continuous or discrete swipe ran in this session.
    gestured: bool,
}

struct PendingTap {
    fingers: usize,
    deadline: Instant,
}

pub struct Engine {
    cfg: Arc<RwLock<Config>>,
    shared: Arc<Mutex<Shared>>,
    mouse: VirtualMouse,
    ax: AxisInfo,
    ay: AxisInfo,
    slots: [Option<Touch>; MAX_SLOTS],
    /// Last position reported in each slot. The kernel suppresses ABS events
    /// whose value equals the slot's cached value, so a new contact may land
    /// without an X or Y event; the previous value is then still correct.
    slot_last: [(i32, i32); MAX_SLOTS],
    current_slot: usize,
    touch_dirty: bool,
    finger_count: usize,
    session: Option<Session>,
    pending_tap: Option<PendingTap>,
    last_tap: Option<(usize, Instant)>,
    compositor_natural: bool,
    /// Max finger travel seen in the frame before all fingers lifted.
    last_travel: f64,
    /// When true, touch data comes from `handle_touch_frame` (hidraw) and
    /// evdev ABS events are ignored.
    hidraw_mode: bool,
}

impl Engine {
    pub fn new(
        cfg: Arc<RwLock<Config>>,
        shared: Arc<Mutex<Shared>>,
        mouse: VirtualMouse,
        ax: AxisInfo,
        ay: AxisInfo,
        compositor_natural: bool,
    ) -> Self {
        Self {
            cfg,
            shared,
            mouse,
            ax,
            ay,
            slots: [None; MAX_SLOTS],
            slot_last: [(0, 0); MAX_SLOTS],
            current_slot: 0,
            touch_dirty: false,
            finger_count: 0,
            session: None,
            pending_tap: None,
            last_tap: None,
            compositor_natural,
            last_travel: 0.0,
            hidraw_mode: false,
        }
    }

    /// Switch the touch source to raw HID reports.
    pub fn set_hidraw_mode(&mut self, on: bool) {
        self.hidraw_mode = on;
        self.slots = [None; MAX_SLOTS];
    }

    /// One decoded HID report: the complete set of contacts right now.
    pub fn handle_touch_frame(&mut self, touches: &[RawTouch]) -> Result<()> {
        let mut next: [Option<Touch>; MAX_SLOTS] = [None; MAX_SLOTS];
        let mut used = 0usize;
        for t in touches.iter().filter(|t| t.down) {
            if used >= MAX_SLOTS {
                break;
            }
            let existing = self.slots.iter().flatten().find(|s| s.id == t.id as i32);
            next[used] = Some(match existing {
                Some(prev) => Touch { id: prev.id, x: t.x, y: t.y, start_x: prev.start_x, start_y: prev.start_y, fresh: false },
                None => Touch { id: t.id as i32, x: t.x, y: t.y, start_x: t.x, start_y: t.y, fresh: true },
            });
            used += 1;
        }
        self.slots = next;
        self.process_frame()
    }

    /// Earliest instant at which `tick` needs to run, if any.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending_tap.as_ref().map(|p| p.deadline)
    }

    /// Time-based work: fire single taps whose double-tap window expired.
    pub fn tick(&mut self, now: Instant) -> Result<()> {
        if let Some(p) = &self.pending_tap {
            if now >= p.deadline {
                let fingers = p.fingers;
                self.pending_tap = None;
                self.fire_rule(GestureKind::Tap, fingers, None)?;
            }
        }
        Ok(())
    }

    pub fn handle(&mut self, ev: InputEvent) -> Result<()> {
        match ev.event_type() {
            EventType::ABSOLUTE => {
                if !self.hidraw_mode {
                    self.handle_abs(ev)
                }
            }
            EventType::RELATIVE => {
                let code = RelativeAxisCode(ev.code());
                let is_wheel = matches!(
                    code,
                    RelativeAxisCode::REL_WHEEL
                        | RelativeAxisCode::REL_HWHEEL
                        | RelativeAxisCode::REL_WHEEL_HI_RES
                        | RelativeAxisCode::REL_HWHEEL_HI_RES
                );
                if is_wheel {
                    let limit = self.cfg.read().unwrap().general.suppress_scroll_from_fingers as usize;
                    if limit > 0 && self.finger_count >= limit {
                        return Ok(());
                    }
                }
                self.mouse.push(ev);
            }
            EventType::KEY => self.mouse.push(ev),
            EventType::SYNCHRONIZATION => {
                if ev.code() == 0 {
                    // SYN_REPORT: end of frame.
                    self.mouse.flush()?;
                    if self.touch_dirty {
                        self.touch_dirty = false;
                        self.process_frame()?;
                    }
                } else if ev.code() == 3 {
                    // SYN_DROPPED: resync by forgetting touches.
                    self.slots = [None; MAX_SLOTS];
                    self.touch_dirty = true;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_abs(&mut self, ev: InputEvent) {
        let code = AbsoluteAxisCode(ev.code());
        let v = ev.value();
        log::trace!("abs {code:?} = {v}");
        match code {
            AbsoluteAxisCode::ABS_MT_SLOT => {
                self.current_slot = (v.max(0) as usize).min(MAX_SLOTS - 1);
            }
            AbsoluteAxisCode::ABS_MT_TRACKING_ID => {
                let slot = &mut self.slots[self.current_slot];
                if v < 0 {
                    *slot = None;
                } else if slot.is_none() {
                    let (x, y) = self.slot_last[self.current_slot];
                    *slot = Some(Touch { id: v, x, y, start_x: x, start_y: y, fresh: true });
                }
                self.touch_dirty = true;
            }
            AbsoluteAxisCode::ABS_MT_POSITION_X => {
                self.slot_last[self.current_slot].0 = v;
                if let Some(t) = &mut self.slots[self.current_slot] {
                    t.x = v;
                    if t.fresh {
                        t.start_x = v;
                    }
                    self.touch_dirty = true;
                }
            }
            AbsoluteAxisCode::ABS_MT_POSITION_Y => {
                self.slot_last[self.current_slot].1 = v;
                if let Some(t) = &mut self.slots[self.current_slot] {
                    t.y = v;
                    if t.fresh {
                        t.start_y = v;
                    }
                    self.touch_dirty = true;
                }
            }
            _ => {}
        }
    }

    fn centroid_mm(&self) -> (usize, (f64, f64)) {
        let mut n = 0usize;
        let (mut sx, mut sy) = (0.0, 0.0);
        for t in self.slots.iter().flatten() {
            n += 1;
            sx += self.ax.to_mm(t.x);
            sy += self.ay.to_mm(t.y);
        }
        if n == 0 {
            (0, (0.0, 0.0))
        } else {
            (n, (sx / n as f64, sy / n as f64))
        }
    }

    /// Largest distance any finger moved from where it landed (mm).
    fn max_travel_mm(&self) -> f64 {
        self.slots
            .iter()
            .flatten()
            .map(|t| {
                let dx = (t.x - t.start_x) as f64 / self.ax.res;
                let dy = (t.y - t.start_y) as f64 / self.ay.res;
                (dx * dx + dy * dy).sqrt()
            })
            .fold(0.0, f64::max)
    }

    fn process_frame(&mut self) -> Result<()> {
        let now = Instant::now();
        let (n, c) = self.centroid_mm();
        let travel = self.max_travel_mm();
        self.finger_count = n;
        for t in self.slots.iter_mut().flatten() {
            t.fresh = false;
        }
        log::trace!("frame: {n} finger(s) centroid=({:.1},{:.1})mm travel={travel:.1}mm", c.0, c.1);

        if n == 0 {
            if let Some(s) = self.session.take() {
                if matches!(s.swipe, SwipeState::Workspace { .. }) {
                    self.shared.lock().unwrap().touchpad.end()?;
                }
                let taps = self.cfg.read().unwrap().taps.clone();
                let dur = now.duration_since(s.start);
                if !s.gestured && dur <= Duration::from_millis(taps.tap_max_ms) && self.last_travel <= taps.tap_max_mm {
                    self.register_tap(s.max_fingers, now, Duration::from_millis(taps.double_tap_ms))?;
                }
            }
            self.last_travel = 0.0;
            return Ok(());
        }
        self.last_travel = travel;

        let mut session = match self.session.take() {
            None => Session {
                start: now,
                max_fingers: n,
                count: n,
                anchor: c,
                last_centroid: c,
                swipe: SwipeState::Armed,
                gestured: false,
            },
            Some(s) => s,
        };

        if session.count != n {
            // Finger count changed: close any running swipe and re-arm.
            if matches!(session.swipe, SwipeState::Workspace { .. }) {
                self.shared.lock().unwrap().touchpad.end()?;
            }
            session.count = n;
            session.max_fingers = session.max_fingers.max(n);
            session.anchor = c;
            session.last_centroid = c;
            session.swipe = SwipeState::Armed;
        }

        match session.swipe {
            SwipeState::Workspace { sign, axis } => {
                let dx = c.0 - session.last_centroid.0;
                let dy = c.1 - session.last_centroid.1;
                let sens = self.cfg.read().unwrap().workspace_swipe.sensitivity;
                let (vx, vy) = match axis {
                    Axis::Horizontal => (dx * sens * sign, 0.0),
                    Axis::Vertical => (0.0, dy * sens * sign),
                };
                if vx != 0.0 || vy != 0.0 {
                    self.shared.lock().unwrap().touchpad.move_by(vx, vy)?;
                }
            }
            SwipeState::Armed => {
                let dx = c.0 - session.anchor.0;
                let dy = c.1 - session.anchor.1;
                let dist = (dx * dx + dy * dy).sqrt();
                let ws = self.cfg.read().unwrap().workspace_swipe.clone();
                if dist >= ws.activation_mm {
                    let dir = if dx.abs() >= dy.abs() {
                        if dx > 0.0 { SwipeDir::Right } else { SwipeDir::Left }
                    } else if dy > 0.0 {
                        SwipeDir::Down
                    } else {
                        SwipeDir::Up
                    };
                    session.gestured = true;
                    if self.fire_rule(GestureKind::Swipe, n, Some(dir))? {
                        session.swipe = SwipeState::Done;
                    } else if ws.enabled && ws.fingers as usize == n && axis_matches(ws.axis, dir) {
                        let natural = ws.direction == Direction::Natural;
                        let compositor_natural = ws.compositor_natural_scroll.unwrap_or(self.compositor_natural);
                        // The compositor already follows the fingers when its
                        // own natural scrolling is on; otherwise flip.
                        let sign = if natural != compositor_natural { -1.0 } else { 1.0 };
                        let mut sh = self.shared.lock().unwrap();
                        sh.touchpad.begin(ws.synthetic_fingers as usize)?;
                        let (vx, vy) = match ws.axis {
                            Axis::Horizontal => (dx * ws.sensitivity * sign, 0.0),
                            Axis::Vertical => (0.0, dy * ws.sensitivity * sign),
                        };
                        sh.touchpad.move_by(vx, vy)?;
                        drop(sh);
                        session.swipe = SwipeState::Workspace { sign, axis: ws.axis };
                        log::debug!("workspace swipe begin: {n} fingers, dir {dir:?}");
                    } else {
                        session.swipe = SwipeState::Done;
                    }
                }
            }
            SwipeState::Done => {}
        }

        session.last_centroid = c;
        self.session = Some(session);
        Ok(())
    }

    fn register_tap(&mut self, fingers: usize, now: Instant, window: Duration) -> Result<()> {
        let cfg = self.cfg.read().unwrap();
        let has_double = cfg.gestures.iter().any(|g| g.enabled && g.kind == GestureKind::DoubleTap && g.fingers as usize == fingers);
        let has_single = cfg.gestures.iter().any(|g| g.enabled && g.kind == GestureKind::Tap && g.fingers as usize == fingers);
        drop(cfg);

        if let Some((f, t)) = self.last_tap {
            if f == fingers && now.duration_since(t) <= window && has_double {
                self.last_tap = None;
                self.pending_tap = None;
                log::debug!("double tap: {fingers} fingers");
                self.fire_rule(GestureKind::DoubleTap, fingers, None)?;
                return Ok(());
            }
        }
        self.last_tap = Some((fingers, now));
        if has_double {
            // Hold the single tap until the double-tap window closes.
            self.pending_tap = if has_single { Some(PendingTap { fingers, deadline: now + window }) } else { None };
        } else if has_single {
            self.fire_rule(GestureKind::Tap, fingers, None)?;
        }
        Ok(())
    }

    /// Run the first enabled rule matching (kind, fingers, direction).
    /// Returns true if a rule fired.
    fn fire_rule(&mut self, kind: GestureKind, fingers: usize, dir: Option<SwipeDir>) -> Result<bool> {
        let action = {
            let cfg = self.cfg.read().unwrap();
            cfg.gestures
                .iter()
                .find(|g| g.enabled && g.kind == kind && g.fingers as usize == fingers && (kind != GestureKind::Swipe || g.direction == dir))
                .map(|g| g.action.clone())
        };
        let Some(action) = action else { return Ok(false) };
        log::info!("gesture {kind:?} {fingers}f {dir:?} -> {action:?}");
        self.run_action(&action)?;
        Ok(true)
    }

    fn run_action(&mut self, action: &Action) -> Result<()> {
        match action {
            Action::Key { combo } => {
                if let Err(e) = self.shared.lock().unwrap().keyboard.tap_combo(combo) {
                    log::error!("key action '{combo}': {e}");
                }
            }
            Action::Button { button } => {
                if let Err(e) = self.mouse.click(button) {
                    log::error!("button action '{button}': {e}");
                }
            }
            Action::Command { command } => {
                if let Err(e) = std::process::Command::new("sh").arg("-c").arg(command).spawn() {
                    log::error!("command '{command}': {e}");
                }
            }
        }
        Ok(())
    }
}

fn axis_matches(axis: Axis, dir: SwipeDir) -> bool {
    match axis {
        Axis::Horizontal => matches!(dir, SwipeDir::Left | SwipeDir::Right),
        Axis::Vertical => matches!(dir, SwipeDir::Up | SwipeDir::Down),
    }
}

impl Engine {
    /// Fingers currently on the surface (for tests and diagnostics).
    #[allow(dead_code)]
    pub fn finger_count(&self) -> usize {
        self.finger_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use evdev::{BusType, InputId};

    fn abs(code: AbsoluteAxisCode, v: i32) -> InputEvent {
        InputEvent::new(EventType::ABSOLUTE.0, code.0, v)
    }
    fn syn() -> InputEvent {
        InputEvent::new(EventType::SYNCHRONIZATION.0, 0, 0)
    }

    fn engine() -> Engine {
        let cfg = Arc::new(RwLock::new(Config::default()));
        let shared = Arc::new(Mutex::new(Shared {
            touchpad: VirtualTouchpad::new().unwrap(),
            keyboard: VirtualKeyboard::new().unwrap(),
        }));
        let mouse = VirtualMouse::new("Test Magic Mouse", InputId::new(BusType::BUS_BLUETOOTH, 0x4c, 0x269, 1)).unwrap();
        let ax = AxisInfo { min: -1100, max: 1258, res: 26.0 };
        let ay = AxisInfo { min: -1589, max: 2047, res: 70.0 };
        Engine::new(cfg, shared, mouse, ax, ay, false)
    }

    #[test]
    fn two_finger_swipe_is_detected() {
        let mut e = engine();
        // Two fingers land.
        for ev in [
            abs(AbsoluteAxisCode::ABS_MT_SLOT, 0),
            abs(AbsoluteAxisCode::ABS_MT_TRACKING_ID, 1),
            abs(AbsoluteAxisCode::ABS_MT_POSITION_X, 0),
            abs(AbsoluteAxisCode::ABS_MT_POSITION_Y, 0),
            abs(AbsoluteAxisCode::ABS_MT_SLOT, 1),
            abs(AbsoluteAxisCode::ABS_MT_TRACKING_ID, 2),
            abs(AbsoluteAxisCode::ABS_MT_POSITION_X, 300),
            abs(AbsoluteAxisCode::ABS_MT_POSITION_Y, 0),
            syn(),
        ] {
            e.handle(ev).unwrap();
        }
        assert_eq!(e.finger_count(), 2);
        assert!(matches!(e.session.as_ref().unwrap().swipe, SwipeState::Armed));
        // Move both fingers 10 mm to the left (260 units).
        for step in 1..=5 {
            for ev in [
                abs(AbsoluteAxisCode::ABS_MT_SLOT, 0),
                abs(AbsoluteAxisCode::ABS_MT_POSITION_X, -52 * step),
                abs(AbsoluteAxisCode::ABS_MT_SLOT, 1),
                abs(AbsoluteAxisCode::ABS_MT_POSITION_X, 300 - 52 * step),
                syn(),
            ] {
                e.handle(ev).unwrap();
            }
        }
        assert!(matches!(e.session.as_ref().unwrap().swipe, SwipeState::Workspace { .. }), "swipe should be running");
        // Lift.
        for ev in [
            abs(AbsoluteAxisCode::ABS_MT_SLOT, 0),
            abs(AbsoluteAxisCode::ABS_MT_TRACKING_ID, -1),
            abs(AbsoluteAxisCode::ABS_MT_SLOT, 1),
            abs(AbsoluteAxisCode::ABS_MT_TRACKING_ID, -1),
            syn(),
        ] {
            e.handle(ev).unwrap();
        }
        assert_eq!(e.finger_count(), 0);
        assert!(e.session.is_none());
    }

    #[test]
    fn two_finger_double_tap_is_detected() {
        let mut e = engine();
        let tap = |e: &mut Engine| {
            for ev in [
                abs(AbsoluteAxisCode::ABS_MT_SLOT, 0),
                abs(AbsoluteAxisCode::ABS_MT_TRACKING_ID, 1),
                abs(AbsoluteAxisCode::ABS_MT_POSITION_X, 0),
                abs(AbsoluteAxisCode::ABS_MT_POSITION_Y, 0),
                abs(AbsoluteAxisCode::ABS_MT_SLOT, 1),
                abs(AbsoluteAxisCode::ABS_MT_TRACKING_ID, 2),
                abs(AbsoluteAxisCode::ABS_MT_POSITION_X, 300),
                abs(AbsoluteAxisCode::ABS_MT_POSITION_Y, 0),
                syn(),
                abs(AbsoluteAxisCode::ABS_MT_SLOT, 0),
                abs(AbsoluteAxisCode::ABS_MT_TRACKING_ID, -1),
                abs(AbsoluteAxisCode::ABS_MT_SLOT, 1),
                abs(AbsoluteAxisCode::ABS_MT_TRACKING_ID, -1),
                syn(),
            ] {
                e.handle(ev).unwrap();
            }
        };
        tap(&mut e);
        assert!(e.last_tap.is_some(), "first tap registered");
        tap(&mut e);
        assert!(e.last_tap.is_none(), "double tap fired and cleared");
    }
}
