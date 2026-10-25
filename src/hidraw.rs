//! Touch data straight from the Magic Mouse's HID reports (via hidraw).
//!
//! The kernel driver's evdev multitouch output is unreliable with the popular
//! DKMS build of hid-magicmouse (every finger ends up in slot 0), so the
//! daemon prefers decoding the raw reports itself. Layout follows
//! hid-magicmouse.c.

use std::path::{Path, PathBuf};

/// Magic Mouse 2 (Bluetooth and USB).
const MOUSE2_REPORT_ID: u8 = 0x12;
/// Original Magic Mouse.
const MOUSE_REPORT_ID: u8 = 0x29;
/// Two reports packed in one packet: [0xf7, len1, report1..., report2...].
const DOUBLE_REPORT_ID: u8 = 0xf7;
const TOUCH_BYTES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawTouch {
    pub id: u8,
    /// Same units as the driver's ABS_MT_POSITION_X/Y.
    pub x: i32,
    pub y: i32,
    pub down: bool,
}

/// Decode one report. Returns `false` if it is not a touch report; `out` is
/// cleared and filled with the touches in the report (down or lifting).
pub fn parse_report(data: &[u8], out: &mut Vec<RawTouch>) -> bool {
    out.clear();
    let Some(&id) = data.first() else { return false };
    let prefix = match id {
        MOUSE2_REPORT_ID => 14,
        MOUSE_REPORT_ID => 6,
        DOUBLE_REPORT_ID => {
            // Only the first inner report is used; the second is a rare
            // repeat and the next read brings fresh data anyway.
            let Some(&len) = data.get(1) else { return false };
            let len = len as usize;
            return data.len() >= 2 + len && parse_report(&data[2..2 + len], out);
        }
        _ => return false,
    };
    if data.len() < prefix || (data.len() - prefix) % TOUCH_BYTES != 0 {
        return false;
    }
    for t in data[prefix..].chunks_exact(TOUCH_BYTES) {
        // [x7..x0] [y3..y0 x11..x8] [y11..y4] [major] [minor] [id1 id0 size..] [orient.. id3 id2] [state..]
        let x12 = ((t[1] as i32 & 0x0f) << 8) | t[0] as i32;
        let y12 = ((t[2] as i32) << 4) | (t[1] as i32 >> 4);
        let x = sign_extend_12(x12);
        let y = -sign_extend_12(y12);
        let id = (((t[6] as u16) << 2) | (t[5] as u16 >> 6)) as u8 & 0x0f;
        let state = t[7] & 0xf0;
        out.push(RawTouch { id, x, y, down: state != 0 });
    }
    true
}

fn sign_extend_12(v: i32) -> i32 {
    if v & 0x800 != 0 { v - 0x1000 } else { v }
}

/// `/dev/hidrawN` belonging to the HID device behind `/dev/input/eventN`.
pub fn hidraw_for_event(event_path: &Path) -> Option<PathBuf> {
    let name = event_path.file_name()?.to_str()?;
    let dir = PathBuf::from(format!("/sys/class/input/{name}/device/device/hidraw"));
    let entry = std::fs::read_dir(dir).ok()?.flatten().next()?;
    Some(PathBuf::from("/dev").join(entry.file_name()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(x: i32, y: i32, id: u8, state: u8) -> [u8; 8] {
        let xu = (x & 0xfff) as u16;
        let yu = ((-y) & 0xfff) as u16;
        [
            (xu & 0xff) as u8,
            ((xu >> 8) as u8 & 0x0f) | (((yu & 0x0f) as u8) << 4),
            (yu >> 4) as u8,
            100,
            90,
            ((id & 0x3) << 6) | 0x20,
            (0x20 << 2) | (id >> 2),
            state,
        ]
    }

    #[test]
    fn decodes_two_fingers() {
        let mut report = vec![MOUSE2_REPORT_ID, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        report.extend_from_slice(&touch(841, -11, 3, 0x40));
        report.extend_from_slice(&touch(-108, 7, 5, 0x30));
        let mut out = Vec::new();
        assert!(parse_report(&report, &mut out));
        assert_eq!(out, vec![
            RawTouch { id: 3, x: 841, y: -11, down: true },
            RawTouch { id: 5, x: -108, y: 7, down: true },
        ]);
    }

    #[test]
    fn lifted_finger_and_empty_report() {
        let mut report = vec![MOUSE2_REPORT_ID; 14];
        report.extend_from_slice(&touch(-750, -130, 1, 0x00));
        let mut out = Vec::new();
        assert!(parse_report(&report, &mut out));
        assert_eq!(out[0], RawTouch { id: 1, x: -750, y: -130, down: false });
        assert!(parse_report(&[MOUSE2_REPORT_ID; 14], &mut out));
        assert!(out.is_empty());
        assert!(!parse_report(&[0x01, 2, 3], &mut out));
    }
}
