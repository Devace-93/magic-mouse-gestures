//! Gesture recognition for one Magic Mouse. Consumes raw evdev events from
//! the grabbed device, forwards pointer/button/wheel events to the virtual
//! mouse, and turns multi-finger touches into gestures.

use crate::config::{Action, Axis, Config, Direction, GestureKind, SwipeDir};
use crate::focus::Focus;
use crate::hidraw::RawTouch;
use crate::virt::{VirtualKeyboard, VirtualMouse, VirtualTouchpad};
use anyhow::Result;
use evdev::{AbsoluteAxisCode, EventType, InputEvent, RelativeAxisCode};
use std::collections::HashMap;
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
    /// Position of `Action::Toggle` / `Action::Cycle` rules, keyed by rule
    /// and focused app. Lives here so it survives a Bluetooth reconnect.
    pub toggles: HashMap<String, usize>,
}

#[derive(Debug)]
enum SwipeState {
    /// Fingers are down but have not travelled far enough yet.
    Armed,
    /// Replaying motion on the virtual touchpad; `sign` flips direction,
    /// `axis` picks the component forwarded.
    Workspace { sign: f64, axis: Axis },
    /// Replaying a pinch: two synthetic contacts whose distance follows the
    /// real finger distance (`sep0` = real distance when the pinch started).
    Pinch { sep0: f64, origin: (f64, f64) },
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
    /// Finger spread (mm) when the current finger count was established.
    sep_anchor: f64,
    last_centroid: (f64, f64),
    swipe: SwipeState,
    /// True once any continuous or discrete swipe ran in this session.
    gestured: bool,
    /// A physical button was pressed while fingers were down: not a tap.
    clicked: bool,
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
    /// Focused window app id (for per-application rules).
    focus: Focus,
    /// Max finger travel seen in the frame before all fingers lifted.
    last_travel: f64,
    /// Centroid speed (mm/s) over the last ~20 ms before all fingers lifted.
    last_speed: f64,
    /// Recent (time, centroid) samples, newest last, about 100 ms deep.
    history: std::collections::VecDeque<(Instant, (f64, f64))>,
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
        focus: Focus,
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
            focus,
            last_travel: 0.0,
            last_speed: 0.0,
            history: std::collections::VecDeque::with_capacity(16),
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
            EventType::KEY => {
                // BTN_* press while touching: the touch is a click, not a tap.
                if ev.value() == 1 && (0x100..0x160).contains(&ev.code()) {
                    if let Some(s) = &mut self.session {
                        s.clicked = true;
                    }
                }
                self.mouse.push(ev)
            }
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

    /// Finger spread (mm): mean distance of the fingers to their centroid,
    /// doubled so that for two fingers it equals the distance between them.
    fn spread_mm(&self, centroid: (f64, f64)) -> f64 {
        let (mut n, mut sum) = (0usize, 0.0);
        for t in self.slots.iter().flatten() {
            let dx = self.ax.to_mm(t.x) - centroid.0;
            let dy = self.ay.to_mm(t.y) - centroid.1;
            sum += (dx * dx + dy * dy).sqrt();
            n += 1;
        }
        if n < 2 { 0.0 } else { 2.0 * sum / n as f64 }
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
                if matches!(s.swipe, SwipeState::Workspace { .. } | SwipeState::Pinch { .. }) {
                    self.shared.lock().unwrap().touchpad.end()?;
                }
                // Lift speed measured up to the lift itself: a finger that
                // rested a while before leaving (no new samples, since the
                // kernel drops unchanged positions) counts as slow.
                if let Some(&(_, c_last)) = self.history.back() {
                    if let Some(&(t0, c0)) = self.history.iter().rev().find(|(t, _)| now.duration_since(*t) >= Duration::from_millis(20)) {
                        let dt = now.duration_since(t0).as_secs_f64().max(0.005);
                        self.last_speed = ((c_last.0 - c0.0).powi(2) + (c_last.1 - c0.1).powi(2)).sqrt() / dt;
                    }
                }
                let taps = self.cfg.read().unwrap().taps.clone();
                let dur = now.duration_since(s.start);
                let is_tap = !s.gestured
                    && !s.clicked
                    && dur <= Duration::from_millis(taps.tap_max_ms)
                    && self.last_travel <= taps.tap_max_mm
                    && self.last_speed <= taps.tap_max_lift_speed;
                log::debug!(
                    "touch ended: {} finger(s), {} ms, travel {:.1} mm, lift speed {:.0} mm/s -> {}",
                    s.max_fingers, dur.as_millis(), self.last_travel, self.last_speed,
                    if is_tap { "tap" } else { "not a tap" }
                );
                if is_tap {
                    self.register_tap(s.max_fingers, now, Duration::from_millis(taps.double_tap_ms))?;
                }
            }
            self.last_travel = 0.0;
            self.last_speed = 0.0;
            self.history.clear();
            return Ok(());
        }
        self.last_travel = travel;
        // Lift speed: displacement between this frame and the most recent
        // sample at least 20 ms older (two reports), so a single noisy frame
        // cannot decide. A tapping finger has settled by now; a flick has not.
        self.history.push_back((now, c));
        while self.history.len() > 1 && now.duration_since(self.history[0].0) > Duration::from_millis(100) {
            self.history.pop_front();
        }
        if let Some(&(t0, c0)) = self.history.iter().rev().find(|(t, _)| now.duration_since(*t) >= Duration::from_millis(20)) {
            let dt = now.duration_since(t0).as_secs_f64().max(0.005);
            self.last_speed = ((c.0 - c0.0).powi(2) + (c.1 - c0.1).powi(2)).sqrt() / dt;
        }

        let spread = self.spread_mm(c);
        let mut session = match self.session.take() {
            None => Session {
                start: now,
                max_fingers: n,
                count: n,
                anchor: c,
                sep_anchor: spread,
                last_centroid: c,
                swipe: SwipeState::Armed,
                gestured: false,
                clicked: false,
            },
            Some(s) => s,
        };

        if session.count != n {
            // Finger count changed: close any running swipe and re-arm.
            if matches!(session.swipe, SwipeState::Workspace { .. } | SwipeState::Pinch { .. }) {
                self.shared.lock().unwrap().touchpad.end()?;
            }
            session.count = n;
            session.max_fingers = session.max_fingers.max(n);
            session.anchor = c;
            session.sep_anchor = spread;
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
                    Axis::Both => (dx * sens * sign, dy * sens * sign),
                };
                if vx != 0.0 || vy != 0.0 {
                    self.shared.lock().unwrap().touchpad.move_by(vx, vy)?;
                }
            }
            SwipeState::Pinch { sep0, origin } => {
                let sens = self.cfg.read().unwrap().pinch.sensitivity;
                let positions = pinch_positions(origin, c, session.anchor, sep0, spread, sens);
                self.shared.lock().unwrap().touchpad.move_to(&positions)?;
            }
            SwipeState::Armed => {
                let dx = c.0 - session.anchor.0;
                let dy = c.1 - session.anchor.1;
                let dist = (dx * dx + dy * dy).sqrt();
                let (ws, pinch) = {
                    let cfg = self.cfg.read().unwrap();
                    (cfg.workspace_swipe.clone(), cfg.pinch.clone())
                };
                let dsep = spread - session.sep_anchor;
                if pinch.enabled && pinch.fingers as usize == n && dsep.abs() >= pinch.activation_mm && dsep.abs() > dist {
                    // Fingers moving apart/together faster than they travel: pinch.
                    let origin = VirtualTouchpad::center_mm();
                    let sep0 = session.sep_anchor.max(1.0);
                    let positions = pinch_positions(origin, c, session.anchor, sep0, spread, pinch.sensitivity);
                    let mut sh = self.shared.lock().unwrap();
                    sh.touchpad.begin_at(&pinch_positions(origin, session.anchor, session.anchor, sep0, sep0, pinch.sensitivity))?;
                    sh.touchpad.move_to(&positions)?;
                    drop(sh);
                    session.gestured = true;
                    session.swipe = SwipeState::Pinch { sep0, origin };
                    log::debug!("pinch begin: {n} fingers, spread {sep0:.1} -> {spread:.1} mm");
                } else if dist >= ws.activation_mm {
                    let dir = if dx.abs() >= dy.abs() {
                        if dx > 0.0 { SwipeDir::Right } else { SwipeDir::Left }
                    } else if dy > 0.0 {
                        SwipeDir::Down
                    } else {
                        SwipeDir::Up
                    };
                    if self.fire_rule(GestureKind::Swipe, n, Some(dir))? {
                        session.gestured = true;
                        session.swipe = SwipeState::Done;
                    } else if ws.enabled && ws.fingers as usize == n && axis_matches(ws.axis, dir) {
                        session.gestured = true;
                        let natural = ws.direction == Direction::Natural;
                        let compositor_natural = ws.compositor_natural_scroll.unwrap_or(self.compositor_natural);
                        // The compositor already follows the fingers when its
                        // own natural scrolling is on; otherwise flip.
                        let sign = if natural != compositor_natural { -1.0 } else { 1.0 };
                        // With `both`, lock to the direction the fingers took first.
                        let axis = match ws.axis {
                            Axis::Both if matches!(dir, SwipeDir::Up | SwipeDir::Down) => Axis::Vertical,
                            Axis::Both => Axis::Horizontal,
                            a => a,
                        };
                        let mut sh = self.shared.lock().unwrap();
                        sh.touchpad.begin(ws.synthetic_fingers as usize)?;
                        let (vx, vy) = match axis {
                            Axis::Horizontal => (dx * ws.sensitivity * sign, 0.0),
                            _ => (0.0, dy * ws.sensitivity * sign),
                        };
                        sh.touchpad.move_by(vx, vy)?;
                        drop(sh);
                        session.swipe = SwipeState::Workspace { sign, axis };
                        log::debug!("workspace swipe begin: {n} fingers, dir {dir:?}");
                    } else {
                        // Nothing to do with this motion; a short touch may
                        // still end as a tap (fingertips drift when tapping).
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
        let focused = self.focus.lock().unwrap().clone().unwrap_or_default().to_ascii_lowercase();
        let action = {
            let cfg = self.cfg.read().unwrap();
            let mut candidates = cfg
                .gestures
                .iter()
                .filter(|g| g.enabled && g.kind == kind && g.fingers as usize == fingers && (kind != GestureKind::Swipe || g.direction == dir));
            // A rule scoped to the focused app wins over a generic one.
            let scoped = candidates
                .clone()
                .find(|g| g.app.as_deref().map_or(false, |a| !focused.is_empty() && focused.contains(&a.to_ascii_lowercase())));
            scoped.or_else(|| candidates.find(|g| g.app.is_none())).map(|g| (g.action.clone(), g.app.clone()))
        };
        let Some((action, app)) = action else { return Ok(false) };
        log::info!("gesture {kind:?} {fingers}f {dir:?} app={app:?} -> {action:?}");
        let toggle_key = format!("{kind:?}/{fingers}/{dir:?}/{app:?}@{focused}");
        self.run_action(&action, &toggle_key)?;
        Ok(true)
    }

    fn run_action(&mut self, action: &Action, toggle_key: &str) -> Result<()> {
        match action {
            Action::Key { combo } => {
                if let Err(e) = self.shared.lock().unwrap().keyboard.tap_combo(combo) {
                    log::error!("key action '{combo}': {e}");
                }
            }
            Action::Toggle { on, off } => {
                let mut sh = self.shared.lock().unwrap();
                let pos = sh.toggles.entry(toggle_key.to_string()).or_insert(0);
                let combo = if *pos % 2 == 0 { on } else { off };
                *pos += 1;
                if let Err(e) = sh.keyboard.tap_combo(combo) {
                    log::error!("toggle action '{combo}': {e}");
                }
            }
            Action::Cycle { steps } => {
                if steps.is_empty() {
                    return Ok(());
                }
                let mut sh = self.shared.lock().unwrap();
                let pos = sh.toggles.entry(toggle_key.to_string()).or_insert(0);
                let combo = &steps[*pos % steps.len()];
                *pos += 1;
                log::debug!("cycle step {}/{}: {combo}", *pos, steps.len());
                if let Err(e) = sh.keyboard.tap_combo(combo) {
                    log::error!("cycle action '{combo}': {e}");
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
        Axis::Both => true,
    }
}

/// Where the two synthetic pinch contacts sit on the pad (mm): centred on
/// `origin` plus the real centroid's displacement, separated by a base
/// distance scaled with the real spread ratio raised to `sensitivity`.
fn pinch_positions(origin: (f64, f64), centroid: (f64, f64), anchor: (f64, f64), sep0: f64, spread: f64, sensitivity: f64) -> [(f64, f64); 2] {
    const BASE_SEP_MM: f64 = 40.0;
    let ratio = (spread.max(0.5) / sep0.max(0.5)).powf(sensitivity);
    let sep = (BASE_SEP_MM * ratio).clamp(4.0, 180.0);
    let cx = origin.0 + (centroid.0 - anchor.0);
    let cy = origin.1 + (centroid.1 - anchor.1);
    [(cx - sep / 2.0, cy), (cx + sep / 2.0, cy)]
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
            toggles: HashMap::new(),
        }));
        let mouse = VirtualMouse::new("Test Magic Mouse", InputId::new(BusType::BUS_BLUETOOTH, 0x4c, 0x269, 1)).unwrap();
        let ax = AxisInfo { min: -1100, max: 1258, res: 26.0 };
        let ay = AxisInfo { min: -1589, max: 2047, res: 70.0 };
        Engine::new(cfg, shared, mouse, ax, ay, false, Arc::new(Mutex::new(None)))
    }

    #[test]
    fn click_during_touch_is_not_a_tap() {
        let mut e = engine();
        for ev in [
            abs(AbsoluteAxisCode::ABS_MT_SLOT, 0),
            abs(AbsoluteAxisCode::ABS_MT_TRACKING_ID, 1),
            abs(AbsoluteAxisCode::ABS_MT_POSITION_X, 0),
            abs(AbsoluteAxisCode::ABS_MT_POSITION_Y, 0),
            syn(),
            InputEvent::new(EventType::KEY.0, 0x110, 1), // BTN_LEFT down
            syn(),
            InputEvent::new(EventType::KEY.0, 0x110, 0),
            syn(),
            abs(AbsoluteAxisCode::ABS_MT_TRACKING_ID, -1),
            syn(),
        ] {
            e.handle(ev).unwrap();
        }
        assert!(e.last_tap.is_none(), "a click must not register as a tap");
    }

    #[test]
    fn two_finger_spread_is_a_pinch() {
        let mut e = engine();
        for ev in [
            abs(AbsoluteAxisCode::ABS_MT_SLOT, 0),
            abs(AbsoluteAxisCode::ABS_MT_TRACKING_ID, 1),
            abs(AbsoluteAxisCode::ABS_MT_POSITION_X, -100),
            abs(AbsoluteAxisCode::ABS_MT_POSITION_Y, 0),
            abs(AbsoluteAxisCode::ABS_MT_SLOT, 1),
            abs(AbsoluteAxisCode::ABS_MT_TRACKING_ID, 2),
            abs(AbsoluteAxisCode::ABS_MT_POSITION_X, 100),
            abs(AbsoluteAxisCode::ABS_MT_POSITION_Y, 0),
            syn(),
        ] {
            e.handle(ev).unwrap();
        }
        // Spread both fingers outwards by 6 mm each (156 units), centroid fixed.
        for step in 1..=4 {
            for ev in [
                abs(AbsoluteAxisCode::ABS_MT_SLOT, 0),
                abs(AbsoluteAxisCode::ABS_MT_POSITION_X, -100 - 39 * step),
                abs(AbsoluteAxisCode::ABS_MT_SLOT, 1),
                abs(AbsoluteAxisCode::ABS_MT_POSITION_X, 100 + 39 * step),
                syn(),
            ] {
                e.handle(ev).unwrap();
            }
        }
        assert!(matches!(e.session.as_ref().unwrap().swipe, SwipeState::Pinch { .. }), "pinch should be running");
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
