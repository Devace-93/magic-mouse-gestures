//! Virtual input devices created through uinput: a pass-through mouse, a
//! touchpad used to replay gestures to the compositor, and a keyboard for
//! shortcut actions.

use anyhow::{anyhow, Context, Result};
use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, EventType, InputEvent, InputId, KeyCode, PropType,
    RelativeAxisCode, UinputAbsSetup,
};
use std::collections::HashMap;

/// Suffix appended to every virtual device name so the daemon never grabs its
/// own devices.
pub const VIRTUAL_SUFFIX: &str = "(mmg)";

#[allow(non_snake_case)]
fn SYN_REPORT() -> InputEvent {
    InputEvent::new(EventType::SYNCHRONIZATION.0, 0, 0)
}

fn key(code: KeyCode, value: i32) -> InputEvent {
    InputEvent::new(EventType::KEY.0, code.0, value)
}
fn abs(code: AbsoluteAxisCode, value: i32) -> InputEvent {
    InputEvent::new(EventType::ABSOLUTE.0, code.0, value)
}

// ---------------------------------------------------------------------------
// Pass-through mouse
// ---------------------------------------------------------------------------

/// Mirrors the real mouse (pointer motion, buttons, wheel) so the compositor
/// keeps working while the real device is grabbed. Keeps the vendor/product
/// and a name containing "Magic Mouse" so hwdb/libinput quirks still apply.
pub struct VirtualMouse {
    dev: VirtualDevice,
    queue: Vec<InputEvent>,
}

impl VirtualMouse {
    pub fn new(real_name: &str, id: InputId) -> Result<Self> {
        let mut keys = AttributeSet::<KeyCode>::new();
        for k in [KeyCode::BTN_LEFT, KeyCode::BTN_RIGHT, KeyCode::BTN_MIDDLE, KeyCode::BTN_SIDE, KeyCode::BTN_EXTRA] {
            keys.insert(k);
        }
        let mut rels = AttributeSet::<RelativeAxisCode>::new();
        for r in [
            RelativeAxisCode::REL_X,
            RelativeAxisCode::REL_Y,
            RelativeAxisCode::REL_WHEEL,
            RelativeAxisCode::REL_HWHEEL,
            RelativeAxisCode::REL_WHEEL_HI_RES,
            RelativeAxisCode::REL_HWHEEL_HI_RES,
        ] {
            rels.insert(r);
        }
        let dev = VirtualDevice::builder()?
            .name(&format!("{real_name} {VIRTUAL_SUFFIX}"))
            .input_id(InputId::new(id.bus_type(), id.vendor(), id.product(), id.version()))
            .with_keys(&keys)?
            .with_relative_axes(&rels)?
            .build()
            .context("creating virtual mouse (is /dev/uinput writable?)")?;
        Ok(Self { dev, queue: Vec::with_capacity(16) })
    }

    pub fn push(&mut self, ev: InputEvent) {
        self.queue.push(ev);
    }

    /// Emit everything queued since the last frame plus a SYN_REPORT.
    pub fn flush(&mut self) -> Result<()> {
        if self.queue.is_empty() {
            return Ok(());
        }
        self.queue.push(SYN_REPORT());
        self.dev.emit(&self.queue)?;
        self.queue.clear();
        Ok(())
    }

    pub fn click(&mut self, button: &str) -> Result<()> {
        let code = match button.to_ascii_lowercase().as_str() {
            "left" => KeyCode::BTN_LEFT,
            "right" => KeyCode::BTN_RIGHT,
            "middle" => KeyCode::BTN_MIDDLE,
            "side" | "back" => KeyCode::BTN_SIDE,
            "extra" | "forward" => KeyCode::BTN_EXTRA,
            other => return Err(anyhow!("unknown button '{other}'")),
        };
        self.dev.emit(&[key(code, 1), SYN_REPORT()])?;
        self.dev.emit(&[key(code, 0), SYN_REPORT()])?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Virtual touchpad
// ---------------------------------------------------------------------------

/// Logical size of the virtual pad in millimetres and its resolution in
/// units per millimetre. Large enough that a long gesture never hits an edge
/// (libinput treats edge touches as palms).
const PAD_W_MM: f64 = 300.0;
const PAD_H_MM: f64 = 200.0;
const PAD_RES: i32 = 20;
const MAX_SLOTS: usize = 5;
/// Horizontal spacing between synthetic fingers.
const FINGER_SPREAD_MM: f64 = 14.0;

pub struct VirtualTouchpad {
    dev: VirtualDevice,
    fingers: usize,
    /// Position of each synthetic finger in pad units (fractional).
    pos: Vec<(f64, f64)>,
    next_tracking_id: i32,
}

impl VirtualTouchpad {
    pub fn new() -> Result<Self> {
        let mut keys = AttributeSet::<KeyCode>::new();
        for k in [
            KeyCode::BTN_TOUCH,
            KeyCode::BTN_TOOL_FINGER,
            KeyCode::BTN_TOOL_DOUBLETAP,
            KeyCode::BTN_TOOL_TRIPLETAP,
            KeyCode::BTN_TOOL_QUADTAP,
            KeyCode::BTN_TOOL_QUINTTAP,
            KeyCode::BTN_LEFT,
        ] {
            keys.insert(k);
        }
        let mut props = AttributeSet::<PropType>::new();
        props.insert(PropType::POINTER);
        props.insert(PropType::BUTTONPAD);

        let max_x = (PAD_W_MM * PAD_RES as f64) as i32;
        let max_y = (PAD_H_MM * PAD_RES as f64) as i32;
        let axis_x = AbsInfo::new(0, 0, max_x, 0, 0, PAD_RES);
        let axis_y = AbsInfo::new(0, 0, max_y, 0, 0, PAD_RES);

        let dev = VirtualDevice::builder()?
            .name(&format!("Magic Mouse Gestures Touchpad {VIRTUAL_SUFFIX}"))
            // Bluetooth bus => libinput treats it as an external touchpad and
            // does not pair it with the keyboard for disable-while-typing.
            .input_id(InputId::new(BusType::BUS_BLUETOOTH, 0x1209, 0x6d67, 1))
            .with_keys(&keys)?
            .with_properties(&props)?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_X, axis_x))?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_Y, axis_y))?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_MT_POSITION_X, axis_x))?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_MT_POSITION_Y, axis_y))?
            .with_absolute_axis(&UinputAbsSetup::new(
                AbsoluteAxisCode::ABS_MT_SLOT,
                AbsInfo::new(0, 0, MAX_SLOTS as i32 - 1, 0, 0, 0),
            ))?
            .with_absolute_axis(&UinputAbsSetup::new(
                AbsoluteAxisCode::ABS_MT_TRACKING_ID,
                AbsInfo::new(0, 0, 65535, 0, 0, 0),
            ))?
            .build()
            .context("creating virtual touchpad (is /dev/uinput writable?)")?;
        Ok(Self { dev, fingers: 0, pos: Vec::new(), next_tracking_id: 1 })
    }


    fn tool_code(n: usize) -> KeyCode {
        match n {
            1 => KeyCode::BTN_TOOL_FINGER,
            2 => KeyCode::BTN_TOOL_DOUBLETAP,
            3 => KeyCode::BTN_TOOL_TRIPLETAP,
            4 => KeyCode::BTN_TOOL_QUADTAP,
            _ => KeyCode::BTN_TOOL_QUINTTAP,
        }
    }

    /// Put `n` fingers down in the middle of the pad.
    pub fn begin(&mut self, n: usize) -> Result<()> {
        if self.fingers > 0 {
            self.end()?;
        }
        let n = n.clamp(1, MAX_SLOTS);
        let cx = PAD_W_MM / 2.0 * PAD_RES as f64;
        let cy = PAD_H_MM / 2.0 * PAD_RES as f64;
        let spread = FINGER_SPREAD_MM * PAD_RES as f64;
        self.pos = (0..n)
            .map(|i| (cx + (i as f64 - (n as f64 - 1.0) / 2.0) * spread, cy))
            .collect();
        let mut evs = Vec::with_capacity(n * 4 + 6);
        for (slot, (x, y)) in self.pos.iter().enumerate() {
            evs.push(abs(AbsoluteAxisCode::ABS_MT_SLOT, slot as i32));
            evs.push(abs(AbsoluteAxisCode::ABS_MT_TRACKING_ID, self.next_tracking_id));
            self.next_tracking_id = (self.next_tracking_id + 1) % 65000;
            evs.push(abs(AbsoluteAxisCode::ABS_MT_POSITION_X, x.round() as i32));
            evs.push(abs(AbsoluteAxisCode::ABS_MT_POSITION_Y, y.round() as i32));
        }
        evs.push(key(KeyCode::BTN_TOUCH, 1));
        evs.push(key(Self::tool_code(n), 1));
        evs.push(abs(AbsoluteAxisCode::ABS_X, self.pos[0].0.round() as i32));
        evs.push(abs(AbsoluteAxisCode::ABS_Y, self.pos[0].1.round() as i32));
        evs.push(SYN_REPORT());
        self.dev.emit(&evs)?;
        self.fingers = n;
        Ok(())
    }

    /// Move all fingers by (dx, dy) millimetres.
    pub fn move_by(&mut self, dx_mm: f64, dy_mm: f64) -> Result<()> {
        if self.fingers == 0 {
            return Ok(());
        }
        let dx = dx_mm * PAD_RES as f64;
        let dy = dy_mm * PAD_RES as f64;
        let max_x = PAD_W_MM * PAD_RES as f64 - 1.0;
        let max_y = PAD_H_MM * PAD_RES as f64 - 1.0;
        let mut evs = Vec::with_capacity(self.fingers * 3 + 3);
        for (slot, p) in self.pos.iter_mut().enumerate() {
            p.0 = (p.0 + dx).clamp(0.0, max_x);
            p.1 = (p.1 + dy).clamp(0.0, max_y);
            evs.push(abs(AbsoluteAxisCode::ABS_MT_SLOT, slot as i32));
            evs.push(abs(AbsoluteAxisCode::ABS_MT_POSITION_X, p.0.round() as i32));
            evs.push(abs(AbsoluteAxisCode::ABS_MT_POSITION_Y, p.1.round() as i32));
        }
        evs.push(abs(AbsoluteAxisCode::ABS_X, self.pos[0].0.round() as i32));
        evs.push(abs(AbsoluteAxisCode::ABS_Y, self.pos[0].1.round() as i32));
        evs.push(SYN_REPORT());
        self.dev.emit(&evs)?;
        Ok(())
    }

    /// Lift all fingers.
    pub fn end(&mut self) -> Result<()> {
        if self.fingers == 0 {
            return Ok(());
        }
        let mut evs = Vec::with_capacity(self.fingers * 2 + 3);
        for slot in 0..self.fingers {
            evs.push(abs(AbsoluteAxisCode::ABS_MT_SLOT, slot as i32));
            evs.push(abs(AbsoluteAxisCode::ABS_MT_TRACKING_ID, -1));
        }
        evs.push(key(Self::tool_code(self.fingers), 0));
        evs.push(key(KeyCode::BTN_TOUCH, 0));
        evs.push(SYN_REPORT());
        self.dev.emit(&evs)?;
        self.fingers = 0;
        self.pos.clear();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Virtual keyboard
// ---------------------------------------------------------------------------

pub struct VirtualKeyboard {
    dev: VirtualDevice,
}

impl VirtualKeyboard {
    pub fn new() -> Result<Self> {
        let mut keys = AttributeSet::<KeyCode>::new();
        // Every ordinary key code (1..=255 covers all keyboard keys).
        for code in 1u16..=255 {
            keys.insert(KeyCode(code));
        }
        let dev = VirtualDevice::builder()?
            .name(&format!("Magic Mouse Gestures Keyboard {VIRTUAL_SUFFIX}"))
            .input_id(InputId::new(BusType::BUS_VIRTUAL, 0x1209, 0x6d68, 1))
            .with_keys(&keys)?
            .build()
            .context("creating virtual keyboard")?;
        Ok(Self { dev })
    }

    /// Press and release a combination such as "super+w" or "ctrl+alt+right".
    pub fn tap_combo(&mut self, combo: &str) -> Result<()> {
        let codes = parse_combo(combo)?;
        let mut press: Vec<InputEvent> = codes.iter().map(|c| key(*c, 1)).collect();
        press.push(SYN_REPORT());
        self.dev.emit(&press)?;
        std::thread::sleep(std::time::Duration::from_millis(15));
        let mut release: Vec<InputEvent> = codes.iter().rev().map(|c| key(*c, 0)).collect();
        release.push(SYN_REPORT());
        self.dev.emit(&release)?;
        Ok(())
    }
}

/// "super+shift+w" -> [KEY_LEFTMETA, KEY_LEFTSHIFT, KEY_W]. Modifiers first,
/// in the order written. A bare modifier ("super") is allowed.
pub fn parse_combo(combo: &str) -> Result<Vec<KeyCode>> {
    let table = key_table();
    let mut out = Vec::new();
    for part in combo.split('+') {
        let name = part.trim().to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }
        let code = if let Some(num) = name.strip_prefix("code:") {
            KeyCode(num.parse::<u16>().with_context(|| format!("bad key code '{name}'"))?)
        } else if let Some(c) = table.get(name.as_str()) {
            *c
        } else {
            return Err(anyhow!("unknown key '{part}' in combo '{combo}'"));
        };
        out.push(code);
    }
    if out.is_empty() {
        return Err(anyhow!("empty key combo"));
    }
    Ok(out)
}

fn key_table() -> HashMap<&'static str, KeyCode> {
    use KeyCode as K;
    let mut m: HashMap<&'static str, KeyCode> = HashMap::new();
    for (names, code) in [
        (&["super", "meta", "win", "cmd", "logo"][..], K::KEY_LEFTMETA),
        (&["ctrl", "control"][..], K::KEY_LEFTCTRL),
        (&["alt", "option"][..], K::KEY_LEFTALT),
        (&["shift"][..], K::KEY_LEFTSHIFT),
        (&["altgr", "ralt"][..], K::KEY_RIGHTALT),
        (&["left"][..], K::KEY_LEFT),
        (&["right"][..], K::KEY_RIGHT),
        (&["up"][..], K::KEY_UP),
        (&["down"][..], K::KEY_DOWN),
        (&["tab"][..], K::KEY_TAB),
        (&["space"][..], K::KEY_SPACE),
        (&["enter", "return"][..], K::KEY_ENTER),
        (&["esc", "escape"][..], K::KEY_ESC),
        (&["backspace"][..], K::KEY_BACKSPACE),
        (&["delete", "del"][..], K::KEY_DELETE),
        (&["home"][..], K::KEY_HOME),
        (&["end"][..], K::KEY_END),
        (&["pageup", "prior"][..], K::KEY_PAGEUP),
        (&["pagedown", "next"][..], K::KEY_PAGEDOWN),
        (&["insert"][..], K::KEY_INSERT),
        (&["minus", "-"][..], K::KEY_MINUS),
        (&["equal", "="][..], K::KEY_EQUAL),
        (&["comma", ","][..], K::KEY_COMMA),
        (&["dot", "period", "."][..], K::KEY_DOT),
        (&["slash", "/"][..], K::KEY_SLASH),
        (&["grave", "`"][..], K::KEY_GRAVE),
        (&["print", "printscreen"][..], K::KEY_SYSRQ),
        (&["volumeup"][..], K::KEY_VOLUMEUP),
        (&["volumedown"][..], K::KEY_VOLUMEDOWN),
        (&["mute"][..], K::KEY_MUTE),
        (&["playpause", "play"][..], K::KEY_PLAYPAUSE),
        (&["nexttrack"][..], K::KEY_NEXTSONG),
        (&["prevtrack"][..], K::KEY_PREVIOUSSONG),
        (&["brightnessup"][..], K::KEY_BRIGHTNESSUP),
        (&["brightnessdown"][..], K::KEY_BRIGHTNESSDOWN),
    ] {
        for n in names {
            m.insert(n, code);
        }
    }
    let letters = [
        ("a", K::KEY_A), ("b", K::KEY_B), ("c", K::KEY_C), ("d", K::KEY_D), ("e", K::KEY_E),
        ("f", K::KEY_F), ("g", K::KEY_G), ("h", K::KEY_H), ("i", K::KEY_I), ("j", K::KEY_J),
        ("k", K::KEY_K), ("l", K::KEY_L), ("m", K::KEY_M), ("n", K::KEY_N), ("o", K::KEY_O),
        ("p", K::KEY_P), ("q", K::KEY_Q), ("r", K::KEY_R), ("s", K::KEY_S), ("t", K::KEY_T),
        ("u", K::KEY_U), ("v", K::KEY_V), ("w", K::KEY_W), ("x", K::KEY_X), ("y", K::KEY_Y),
        ("z", K::KEY_Z),
        ("0", K::KEY_0), ("1", K::KEY_1), ("2", K::KEY_2), ("3", K::KEY_3), ("4", K::KEY_4),
        ("5", K::KEY_5), ("6", K::KEY_6), ("7", K::KEY_7), ("8", K::KEY_8), ("9", K::KEY_9),
        ("f1", K::KEY_F1), ("f2", K::KEY_F2), ("f3", K::KEY_F3), ("f4", K::KEY_F4),
        ("f5", K::KEY_F5), ("f6", K::KEY_F6), ("f7", K::KEY_F7), ("f8", K::KEY_F8),
        ("f9", K::KEY_F9), ("f10", K::KEY_F10), ("f11", K::KEY_F11), ("f12", K::KEY_F12),
    ];
    for (n, c) in letters {
        m.insert(n, c);
    }
    m
}
