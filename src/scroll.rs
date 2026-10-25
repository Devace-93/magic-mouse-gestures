//! Scroll-start filter for the Magic Mouse.
//!
//! The kernel driver turns one-finger motion into wheel events after only
//! ~1.3 mm of travel. A fingertip landing on the curved shell rolls a little
//! before the hand actually moves, and that roll often points the other
//! way, so a scroll regularly starts with a short jump in the wrong
//! direction. The gate below holds the first wheel events of every touch,
//! sums them, and only starts forwarding once the net movement on an axis
//! is clearly in one direction. The opposite-direction part is dropped;
//! after that the stream passes through untouched, so deliberate reversals
//! still work.

use crate::config::Scroll;
use evdev::{EventType, InputEvent, RelativeAxisCode};

/// One wheel notch in high-resolution units (Linux convention).
pub const HI_RES_NOTCH: i32 = 120;

#[derive(Debug, Default, Clone, Copy)]
struct AxisGate {
    /// Pending low-resolution (REL_WHEEL / REL_HWHEEL) sum.
    lo: i32,
    /// Pending high-resolution (REL_*_HI_RES) sum.
    hi: i32,
    /// Direction decided for this touch (+1 / -1); `None` while holding.
    committed: Option<i32>,
    /// Events held back and never forwarded (for the debug log only).
    dropped: u32,
}

#[derive(Debug, Default)]
pub struct ScrollGate {
    /// Index 0 = vertical wheel, 1 = horizontal wheel.
    axes: [AxisGate; 2],
}

impl ScrollGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// A new touch started: forget the previous direction, hold again.
    pub fn reset(&mut self) {
        self.axes = [AxisGate::default(); 2];
    }

    /// Feed one wheel event. Pushes the events to forward now into `out`
    /// (none while the direction is still undecided, the accumulated net
    /// movement once it is, the event itself afterwards).
    pub fn feed(&mut self, ev: InputEvent, cfg: &Scroll, out: &mut Vec<InputEvent>) {
        let code = RelativeAxisCode(ev.code());
        let (axis, hi_res) = match code {
            RelativeAxisCode::REL_WHEEL => (0, false),
            RelativeAxisCode::REL_WHEEL_HI_RES => (0, true),
            RelativeAxisCode::REL_HWHEEL => (1, false),
            RelativeAxisCode::REL_HWHEEL_HI_RES => (1, true),
            _ => {
                out.push(ev);
                return;
            }
        };
        if !cfg.direction_debounce {
            out.push(ev);
            return;
        }
        let g = &mut self.axes[axis];
        if g.committed.is_some() {
            out.push(ev);
            return;
        }
        if hi_res {
            g.hi += ev.value();
        } else {
            g.lo += ev.value();
        }
        g.dropped += 1;
        let threshold = cfg.debounce_units.max(1);
        let lo_threshold = (threshold + HI_RES_NOTCH - 1) / HI_RES_NOTCH;
        let decided = if g.hi.abs() >= threshold {
            Some(g.hi.signum())
        } else if g.lo.abs() >= lo_threshold {
            Some(g.lo.signum())
        } else {
            None
        };
        let Some(sign) = decided else { return };
        g.committed = Some(sign);
        log::debug!(
            "scroll gate: {} axis locked {} after {} event(s), net lo={} hi={}",
            if axis == 0 { "vertical" } else { "horizontal" },
            if sign > 0 { "+" } else { "-" },
            g.dropped,
            g.lo,
            g.hi
        );
        // Forward the net movement; anything that pointed the other way has
        // cancelled out and is gone.
        let (lo_code, hi_code) = if axis == 0 {
            (RelativeAxisCode::REL_WHEEL, RelativeAxisCode::REL_WHEEL_HI_RES)
        } else {
            (RelativeAxisCode::REL_HWHEEL, RelativeAxisCode::REL_HWHEEL_HI_RES)
        };
        if g.lo != 0 {
            out.push(InputEvent::new(EventType::RELATIVE.0, lo_code.0, g.lo));
        }
        if g.hi != 0 {
            out.push(InputEvent::new(EventType::RELATIVE.0, hi_code.0, g.hi));
        }
        g.lo = 0;
        g.hi = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wheel_hi(v: i32) -> InputEvent {
        InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_WHEEL_HI_RES.0, v)
    }
    fn wheel_lo(v: i32) -> InputEvent {
        InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_WHEEL.0, v)
    }
    fn values(out: &[InputEvent]) -> Vec<(u16, i32)> {
        out.iter().map(|e| (e.code(), e.value())).collect()
    }

    #[test]
    fn opposite_blip_at_start_is_dropped() {
        let cfg = Scroll::default();
        let mut gate = ScrollGate::new();
        let mut out = Vec::new();
        // Landing roll: three steps the wrong way.
        for _ in 0..3 {
            gate.feed(wheel_hi(-12), &cfg, &mut out);
        }
        assert!(out.is_empty(), "held while undecided");
        // Real movement: 13 steps up (156 units).
        for _ in 0..13 {
            gate.feed(wheel_hi(12), &cfg, &mut out);
        }
        // Net +120 reached at step 13; the blip is gone, nothing negative was sent.
        assert!(out.iter().all(|e| e.value() > 0), "{:?}", values(&out));
        assert_eq!(out.iter().map(|e| e.value()).sum::<i32>(), 120);
        // Afterwards events pass through unchanged, including reversals.
        out.clear();
        gate.feed(wheel_hi(-12), &cfg, &mut out);
        assert_eq!(values(&out), vec![(RelativeAxisCode::REL_WHEEL_HI_RES.0, -12)]);
    }

    #[test]
    fn reset_holds_again_for_the_next_touch() {
        let cfg = Scroll::default();
        let mut gate = ScrollGate::new();
        let mut out = Vec::new();
        for _ in 0..10 {
            gate.feed(wheel_hi(12), &cfg, &mut out);
        }
        assert_eq!(out.len(), 1);
        gate.reset();
        out.clear();
        gate.feed(wheel_hi(12), &cfg, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn low_res_notch_decides_without_hi_res() {
        let cfg = Scroll::default();
        let mut gate = ScrollGate::new();
        let mut out = Vec::new();
        gate.feed(wheel_lo(-1), &cfg, &mut out);
        assert_eq!(values(&out), vec![(RelativeAxisCode::REL_WHEEL.0, -1)]);
    }

    #[test]
    fn disabled_passes_everything() {
        let cfg = Scroll { direction_debounce: false, ..Scroll::default() };
        let mut gate = ScrollGate::new();
        let mut out = Vec::new();
        gate.feed(wheel_hi(-12), &cfg, &mut out);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn axes_are_independent() {
        let cfg = Scroll::default();
        let mut gate = ScrollGate::new();
        let mut out = Vec::new();
        for _ in 0..10 {
            gate.feed(wheel_hi(12), &cfg, &mut out);
        }
        assert_eq!(out.len(), 1);
        out.clear();
        gate.feed(InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_HWHEEL_HI_RES.0, 12), &cfg, &mut out);
        assert!(out.is_empty(), "horizontal still undecided");
    }
}
