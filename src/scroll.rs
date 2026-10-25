//! Wheel shaping for the Magic Mouse's one-finger scrolling.
//!
//! The kernel driver turns finger motion into wheel events, and the daemon
//! re-emits them through the virtual mouse. Two things are fixed on the way:
//!
//! * **Direction debounce.** The driver starts scrolling after only ~1.3 mm
//!   of travel, and a fingertip landing on the curved shell rolls about that
//!   far before the hand moves, often the other way. The first wheel events
//!   of every touch are held and summed; forwarding starts once the net
//!   movement on an axis is clearly one way, and the opposite part is gone.
//!
//! * **Consistent resolution.** The driver emits low-resolution notches
//!   (`REL_WHEEL`, one per ~1.4 mm, starting after 2.9 mm) and
//!   high-resolution steps (`REL_WHEEL_HI_RES`, 1/10 notch per ~0.13 mm,
//!   starting after 1.3 mm) from two separate counters, so they disagree and
//!   a fast flick can produce a frame with notches but no hi-res step. When
//!   that is the first wheel frame libinput ever sees from a device it
//!   switches that device to "kernel bug" emulation for good, and from then
//!   on scrolling feels different (whole notches instead of smooth steps).
//!   Which mode you got depended on the first scroll after the mouse
//!   connected. The shaper therefore ignores the driver's notches, keeps one
//!   hi-res counter per axis, and emits both resolutions from it, hi-res
//!   first in every frame.
//!
//! `mode = "notches"` releases movement in whole notches (what terminals and
//! most apps treat as "scroll three lines"); `mode = "smooth"` releases every
//! hi-res step as it comes. `speed` scales the movement before either.

use crate::config::{Scroll, ScrollMode};
use evdev::{EventType, InputEvent, RelativeAxisCode};

/// One wheel notch in high-resolution units (Linux convention).
pub const HI_RES_NOTCH: i32 = 120;

#[derive(Debug, Default, Clone, Copy)]
struct AxisGate {
    /// Low-resolution notches seen from the driver while undecided (only
    /// used when no hi-res step arrived at all, e.g. a hard flick).
    lo_seen: i32,
    /// Pending movement in hi-res units, already scaled by `speed`.
    acc: f64,
    /// Direction decided for this touch (+1 / -1); `None` while holding.
    committed: Option<i32>,
    /// Events held so far (for the debug log only).
    held: u32,
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
        // Leftover sub-notch movement is dropped too, so a new touch never
        // releases a notch early because of the previous scroll.
        self.axes = [AxisGate::default(); 2];
    }

    /// Feed one wheel event. Pushes the events to forward now into `out`.
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
        let speed = if cfg.speed > 0.0 { cfg.speed } else { 1.0 };
        let g = &mut self.axes[axis];
        if hi_res {
            g.acc += ev.value() as f64 * speed;
        } else if g.committed.is_none() {
            g.lo_seen += ev.value();
        }
        // After the decision the driver's own notches are ignored: they are
        // regenerated from the hi-res counter below.

        if g.committed.is_none() {
            g.held += 1;
            if !cfg.direction_debounce {
                g.committed = Some(0);
            } else {
                let threshold = cfg.debounce_units.max(1) as f64;
                let decided = if g.acc.abs() >= threshold {
                    Some(g.acc.signum() as i32)
                } else if g.acc == 0.0 && g.lo_seen != 0 {
                    // A hard flick: the driver's hi-res counter missed the
                    // first frame entirely. Use its notches instead.
                    g.acc = (g.lo_seen * HI_RES_NOTCH) as f64 * speed;
                    Some(g.lo_seen.signum())
                } else {
                    None
                };
                let Some(sign) = decided else { return };
                g.committed = Some(sign);
                log::debug!(
                    "scroll gate: {} axis locked {} after {} event(s), net {:.0} hi-res units",
                    if axis == 0 { "vertical" } else { "horizontal" },
                    if sign > 0 { "+" } else { "-" },
                    g.held,
                    g.acc
                );
            }
        }
        g.lo_seen = 0;

        let (lo_code, hi_code) = if axis == 0 {
            (RelativeAxisCode::REL_WHEEL, RelativeAxisCode::REL_WHEEL_HI_RES)
        } else {
            (RelativeAxisCode::REL_HWHEEL, RelativeAxisCode::REL_HWHEEL_HI_RES)
        };
        match cfg.mode {
            ScrollMode::Notches => {
                let notches = (g.acc / HI_RES_NOTCH as f64).trunc() as i32;
                if notches != 0 {
                    g.acc -= (notches * HI_RES_NOTCH) as f64;
                    out.push(InputEvent::new(EventType::RELATIVE.0, hi_code.0, notches * HI_RES_NOTCH));
                    out.push(InputEvent::new(EventType::RELATIVE.0, lo_code.0, notches));
                }
            }
            ScrollMode::Smooth => {
                let units = g.acc.trunc() as i32;
                if units != 0 {
                    g.acc -= units as f64;
                    out.push(InputEvent::new(EventType::RELATIVE.0, hi_code.0, units));
                }
            }
        }
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
    const HI: u16 = RelativeAxisCode::REL_WHEEL_HI_RES.0;
    const LO: u16 = RelativeAxisCode::REL_WHEEL.0;

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
        // Real movement: 13 steps up (156 units); net reaches +120 at step 13.
        for _ in 0..13 {
            gate.feed(wheel_hi(12), &cfg, &mut out);
        }
        assert_eq!(values(&out), vec![(HI, 120), (LO, 1)], "one notch, hi-res first, nothing negative");
        // Afterwards a reversal is honoured once it adds up to a notch.
        out.clear();
        for _ in 0..10 {
            gate.feed(wheel_hi(-12), &cfg, &mut out);
        }
        assert_eq!(values(&out), vec![(HI, -120), (LO, -1)]);
    }

    #[test]
    fn driver_notches_are_ignored_once_hi_res_flows() {
        let cfg = Scroll::default();
        let mut gate = ScrollGate::new();
        let mut out = Vec::new();
        for _ in 0..10 {
            gate.feed(wheel_hi(12), &cfg, &mut out);
        }
        out.clear();
        gate.feed(wheel_lo(1), &cfg, &mut out);
        assert!(out.is_empty(), "driver notch must not be forwarded twice");
    }

    #[test]
    fn hard_flick_without_hi_res_still_scrolls() {
        let cfg = Scroll::default();
        let mut gate = ScrollGate::new();
        let mut out = Vec::new();
        gate.feed(wheel_lo(-3), &cfg, &mut out);
        assert_eq!(values(&out), vec![(HI, -360), (LO, -3)]);
    }

    #[test]
    fn smooth_mode_forwards_each_step() {
        let cfg = Scroll { mode: ScrollMode::Smooth, ..Scroll::default() };
        let mut gate = ScrollGate::new();
        let mut out = Vec::new();
        for _ in 0..10 {
            gate.feed(wheel_hi(12), &cfg, &mut out);
        }
        assert_eq!(values(&out), vec![(HI, 120)]);
        out.clear();
        gate.feed(wheel_hi(12), &cfg, &mut out);
        assert_eq!(values(&out), vec![(HI, 12)]);
    }

    #[test]
    fn speed_scales_movement() {
        let cfg = Scroll { speed: 0.5, ..Scroll::default() };
        let mut gate = ScrollGate::new();
        let mut out = Vec::new();
        for _ in 0..10 {
            gate.feed(wheel_hi(12), &cfg, &mut out);
        }
        assert!(out.is_empty(), "60 scaled units is below the threshold");
        for _ in 0..10 {
            gate.feed(wheel_hi(12), &cfg, &mut out);
        }
        assert_eq!(values(&out), vec![(HI, 120), (LO, 1)]);
    }

    #[test]
    fn reset_holds_again_for_the_next_touch() {
        let cfg = Scroll::default();
        let mut gate = ScrollGate::new();
        let mut out = Vec::new();
        for _ in 0..10 {
            gate.feed(wheel_hi(12), &cfg, &mut out);
        }
        assert_eq!(out.len(), 2);
        gate.reset();
        out.clear();
        gate.feed(wheel_hi(12), &cfg, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn debounce_off_still_shapes() {
        let cfg = Scroll { direction_debounce: false, ..Scroll::default() };
        let mut gate = ScrollGate::new();
        let mut out = Vec::new();
        gate.feed(wheel_hi(-12), &cfg, &mut out);
        assert!(out.is_empty(), "notches mode waits for a whole notch");
        for _ in 0..9 {
            gate.feed(wheel_hi(-12), &cfg, &mut out);
        }
        assert_eq!(values(&out), vec![(HI, -120), (LO, -1)]);
    }

    #[test]
    fn axes_are_independent() {
        let cfg = Scroll::default();
        let mut gate = ScrollGate::new();
        let mut out = Vec::new();
        for _ in 0..10 {
            gate.feed(wheel_hi(12), &cfg, &mut out);
        }
        assert_eq!(out.len(), 2);
        out.clear();
        gate.feed(InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_HWHEEL_HI_RES.0, 12), &cfg, &mut out);
        assert!(out.is_empty(), "horizontal still undecided");
    }
}
