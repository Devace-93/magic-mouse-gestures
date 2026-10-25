#!/usr/bin/env bash
# Install magic-mouse-gestures system-wide (binary, udev rule)
# and enable the per-user background service.
set -euo pipefail
cd "$(dirname "$0")"

PREFIX=${PREFIX:-/usr/local}
BIN=target/release/magic-mouse-gestures

need() { command -v "$1" >/dev/null 2>&1 || { echo "missing: $1" >&2; exit 1; }; }

if [[ ! -x $BIN ]]; then
  need cargo
  echo ">> building release binary"
  cargo build --release
fi

echo ">> installing to $PREFIX (sudo)"
sudo install -Dm755 "$BIN" "$PREFIX/bin/magic-mouse-gestures"
sudo install -Dm644 packaging/60-magic-mouse-gestures.rules /etc/udev/rules.d/60-magic-mouse-gestures.rules
sudo install -Dm644 packaging/magic-mouse-gestures.conf /etc/modules-load.d/magic-mouse-gestures.conf
sudo sed "s|/usr/local/bin|$PREFIX/bin|" packaging/magic-mouse-gestures.service | sudo tee /etc/systemd/user/magic-mouse-gestures.service >/dev/null
sudo modprobe uinput || true
sudo udevadm control --reload-rules
sudo udevadm trigger --subsystem-match=input --subsystem-match=misc

echo ">> enabling user service"
pkill -f '^[^ ]*magic-mouse-gestures run' 2>/dev/null || true   # stop any manually started instance
systemctl --user daemon-reload
systemctl --user enable --now magic-mouse-gestures.service
sleep 1
systemctl --user --no-pager --lines=5 status magic-mouse-gestures.service || true

cat <<MSG

Done. Swipe left/right with two fingers on the Magic Mouse to switch
workspaces; double-tap with two fingers to show them.
Config:      ~/.config/magic-mouse-gestures/config.toml (reloaded on save)
Logs:        journalctl --user -fu magic-mouse-gestures
MSG
