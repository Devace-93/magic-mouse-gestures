# magic-mouse-gestures

Native-feeling multitouch gestures for the Apple Magic Mouse on Linux
(Wayland), implemented as a small userspace driver.

Out of the box:

| Gesture on the mouse       | Result                                                        |
|----------------------------|---------------------------------------------------------------|
| 2-finger swipe left/right  | Switch workspace, animated and following your fingers         |
| 2-finger double tap        | Show the workspaces (`Super+W` on COSMIC, `Super` on GNOME)   |
| 1-finger scroll, clicks    | Unchanged (handled by the kernel `hid-magicmouse` driver)     |

Extra taps and swipes (3 fingers, vertical swipes, ...) can be mapped to
shortcuts, mouse buttons or commands in the config file.

## How it works

The Magic Mouse reports every finger on its surface in its HID reports, but
desktops ignore that because the device is a *mouse*, not a touchpad. This
daemon:

1. Grabs the mouse's evdev node and re-emits pointer motion, buttons and
   wheel events through a virtual mouse, so everything keeps working.
2. Decodes the finger positions from the raw HID reports (`/dev/hidrawN`,
   same layout the kernel driver uses). It falls back to the driver's evdev
   multitouch events when hidraw is not accessible; note that the popular DKMS
   build of `hid-magicmouse` (4.18+magicmouse2) reports every finger in slot 0,
   so the hidraw path is what makes gestures reliable there.
3. Tracks the fingers on the surface. When two fingers travel sideways it puts
   **four synthetic fingers** on a virtual touchpad and replays the motion.
   The compositor sees an ordinary 4-finger touchpad swipe and runs its own
   workspace animation: it follows the fingers, pauses when they pause, goes
   back when they go back, and uses the release velocity to settle.
4. Recognises taps and discrete swipes and fires configurable actions through
   a virtual keyboard / the virtual mouse / a shell command.

Nothing is patched in the kernel or the compositor, so it survives updates
and works on any compositor with libinput touchpad gestures (COSMIC, GNOME,
KDE, niri, ...).

## Install

Requirements: Rust toolchain (to build), `/dev/uinput` (standard), the
`hid-magicmouse` kernel module (in-tree; a DKMS version also works).

```sh
git clone https://github.com/Devace-93/magic-mouse-gestures
cd magic-mouse-gestures
./install.sh
```

`install.sh` builds the binary, installs it to `/usr/local/bin`, adds a udev
rule granting the logged-in user access to the mouse's input and hidraw nodes
and to `/dev/uinput`, and enables a per-user systemd service. Remove everything with `./uninstall.sh`.

If the Magic Mouse was paired before the udev rule was installed, unplug/re-
pair it or run `sudo udevadm trigger` once.

## Configure

The daemon writes a default `~/.config/magic-mouse-gestures/config.toml` on
first start and reloads it whenever it changes.

- **Direction** — *Natural* (macOS-like, swipe left = next workspace) or
  *Inverted*. The daemon accounts for your desktop's touchpad natural-scroll
  setting automatically.
- **Sensitivity** — finger travel per workspace.
- **Desktop gesture fingers** — 4 for COSMIC/KDE, 3 for GNOME.
- **Gestures** — add rows like *Tap · 3 fingers → Click middle* or
  *Swipe · 2 fingers · up → super+w*.

```toml
[workspace_swipe]
enabled = true
fingers = 2
direction = "natural"   # or "inverted"
sensitivity = 0.4
synthetic_fingers = 4   # 3 on GNOME
axis = "horizontal"

[[gesture]]
kind = "double_tap"     # swipe | tap | double_tap
fingers = 2
action = { type = "key", combo = "super+w" }

[[gesture]]
kind = "swipe"
fingers = 3
direction = "up"
action = { type = "command", command = "notify-send 'three fingers up'" }
```

## Troubleshooting

```sh
magic-mouse-gestures probe          # is the mouse seen, with touch axes?
magic-mouse-gestures watch          # live finger positions
magic-mouse-gestures demo-swipe left 40   # replay a synthetic 4-finger swipe
journalctl --user -fu magic-mouse-gestures
```

- *No device found*: the mouse must be connected over **Bluetooth** (the
  Lightning cable only charges on Linux) and bound to `hid-magicmouse`.
- *Swipe goes the wrong way*: flip **Direction**, or set *Desktop touchpad
  natural scrolling* explicitly.
- *Nothing happens on swipe*: check the desktop handles 4-finger (or
  3-finger) touchpad swipes and that it does not disable touchpads while a
  mouse is connected.
- *Permission denied*: make sure the udev rule is installed (`install.sh`) or
  add yourself to the `input` group.

## Development

```sh
cargo build --release
./target/release/magic-mouse-gestures -v run
```

Layout: `src/main.rs` (device discovery, event loop), `src/engine.rs`
(gesture recognition), `src/hidraw.rs` (HID report decoding), `src/virt.rs`
(uinput devices), `src/config.rs`, `packaging/` (systemd unit, udev rule).

## License

MIT
