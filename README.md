# magic-mouse-gestures

Native-feeling multitouch gestures for the Apple Magic Mouse on Linux
(Wayland), implemented as a small userspace driver.

Out of the box:

| Gesture on the mouse       | Result                                                        |
|----------------------------|---------------------------------------------------------------|
| 2-finger swipe left/right  | Switch workspace, animated and following your fingers         |
| 2-finger pinch             | Native pinch zoom in apps that support it (browsers, viewers) |
| 2-finger double tap        | Show the workspaces (`Super+W` on COSMIC, `Super` on GNOME)   |
| 3-finger tap               | Show all applications (`Super+A` app library / app grid)      |
| 1-finger scroll, clicks    | Unchanged, except the backwards jump a scroll often starts with is filtered out |

Extra taps and swipes (3 fingers, vertical swipes, ...) can be mapped to
shortcuts, mouse buttons or commands in the config file, optionally only for
a given application (e.g. 2-finger swipes = back/forward in the browser).

## How it works

The Magic Mouse reports every finger on its surface in its HID reports, but
desktops ignore that because the device is a *mouse*, not a touchpad. This
daemon:

1. Grabs the mouse's evdev node and re-emits pointer motion, buttons and
   wheel events through a virtual mouse, so everything keeps working. The
   first wheel events of each touch are held until the net movement clearly
   points one way, which removes the short backwards jump the kernel driver
   produces while the fingertip settles on the shell.
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
   When two fingers move apart or together it replays **two contacts
   spreading** instead, which libinput reports as a pinch and the compositor
   forwards to the focused app as native pinch zoom.
4. Recognises taps and discrete swipes and fires configurable actions through
   a virtual keyboard / the virtual mouse / a shell command. Rules can be
   scoped to the focused application; the app id comes from the compositor's
   toplevel protocols (wlr-foreign-toplevel-management on KDE/Sway/Hyprland,
   ext-foreign-toplevel-list + cosmic-toplevel-info on COSMIC; GNOME exposes
   neither, so there `app` rules are simply never matched).

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

[pinch]
enabled = true
sensitivity = 1.5       # zoom aggressiveness

# One-finger scrolling starts with a short jump the wrong way on a Magic
# Mouse: the kernel driver begins scrolling after ~1.3 mm of travel, and a
# fingertip landing on the curved shell rolls about that far before the hand
# moves. The daemon holds the first wheel events of each touch until the net
# movement is clearly one way (120 units = one notch) and drops the rest.
[scroll]
direction_debounce = true
debounce_units = 120    # raise if a scroll still starts backwards, lower if it starts late

# Optional macOS-style smart zoom (off by default: a one-finger double tap is
# easy to trigger by accident). Each double tap sends the next step, wrapping
# around; browsers step 100 -> 110 -> 125 -> 150 -> 175 -> 200 per Ctrl+KP_Plus.
[[gesture]]
kind = "double_tap"
fingers = 1
action = { type = "cycle", steps = ["ctrl+kpplus,ctrl+kpplus", "ctrl+kpplus,ctrl+kpplus", "ctrl+kpplus", "ctrl+0"] }

# Alternative: a plain toggle that magnifies the whole screen with COSMIC's
# accessibility zoom instead of the app's own zoom.
# action = { type = "toggle", on = "super+equal", off = "super+minus" }

[[gesture]]
kind = "swipe"
fingers = 3
direction = "up"
action = { type = "command", command = "notify-send 'three fingers up'" }

# Per-application: in Firefox a 2-finger swipe goes back/forward instead of
# switching workspaces (app-scoped rules win over the workspace swipe).
[[gesture]]
kind = "swipe"
fingers = 2
direction = "right"
app = "firefox"
action = { type = "key", combo = "alt+left" }

[[gesture]]
kind = "swipe"
fingers = 2
direction = "left"
app = "firefox"
action = { type = "key", combo = "alt+right" }
```

On GNOME set `axis = "both"` under `[workspace_swipe]` (with
`synthetic_fingers = 3`): a horizontal 2-finger swipe switches workspaces and a
vertical one opens the overview, both with GNOME's own animation.

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
- *Pinch zooms the wrong amount*: adjust `pinch.sensitivity` (1.0 = touchpad
  parity).
- *`app` rules never fire*: run with `-v` and look for "focus tracking via";
  the compositor must expose one of the toplevel protocols listed above.

## Development

```sh
cargo build --release
./target/release/magic-mouse-gestures -v run
```

Layout: `src/main.rs` (device discovery, event loop), `src/engine.rs`
(gesture recognition), `src/hidraw.rs` (HID report decoding), `src/focus.rs`
(focused-app tracking over Wayland), `src/virt.rs` (uinput devices),
`src/config.rs`, `packaging/` (systemd unit, udev rule).

## Related work

Other people have attacked the same problem; worth a look to compare approaches:

- [Salacfrantisek/magic-mouse-wayland-gestures](https://github.com/Salacfrantisek/magic-mouse-wayland-gestures)
  independently uses the same idea (hidraw in, gesture-only virtual touchpad
  out) for GNOME, and adds one-finger scroll shaping and two-finger pinch zoom.
- [brenoperucchi/magic-mouse-gestures](https://github.com/brenoperucchi/magic-mouse-gestures)
  reads hidraw and maps two-finger swipes to keystrokes (`wtype`).
- [isomoes/mouse-gesture](https://github.com/isomoes/mouse-gesture) is a Rust
  evdev recognizer for the USB-C model with `xdotool` actions (X11).
- [entrope/linux-magicmouse](https://github.com/entrope/linux-magicmouse) is the
  origin of the `hid-magicmouse` kernel driver.

What this project adds on top: compositor-native, finger-following animation
that is not tied to GNOME (configurable 3/4 synthetic fingers), a
driver-independent hidraw decoder with an evdev fallback, a generic rule engine
(tap / double tap / swipe x N fingers -> key, button or command), hot-reloaded
configuration and a settings UI.

## License

MIT
