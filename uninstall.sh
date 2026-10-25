#!/usr/bin/env bash
# Remove magic-mouse-gestures. Keeps ~/.config/magic-mouse-gestures unless --purge.
set -euo pipefail
PREFIX=${PREFIX:-/usr/local}
systemctl --user disable --now magic-mouse-gestures.service 2>/dev/null || true
sudo rm -f "$PREFIX/bin/magic-mouse-gestures" \
  /etc/udev/rules.d/60-magic-mouse-gestures.rules /etc/modules-load.d/magic-mouse-gestures.conf \
  /etc/systemd/user/magic-mouse-gestures.service
sudo udevadm control --reload-rules
systemctl --user daemon-reload
[[ ${1:-} == --purge ]] && rm -rf "${XDG_CONFIG_HOME:-$HOME/.config}/magic-mouse-gestures"
echo "removed"
