#!/usr/bin/env bash
# Remove mousetouch. Keeps ~/.config/mousetouch unless --purge.
set -euo pipefail
PREFIX=${PREFIX:-/usr/local}
systemctl --user disable --now mousetouch.service 2>/dev/null || true
sudo rm -f "$PREFIX/bin/mousetouch" "$PREFIX/bin/mousetouch-settings" \
  "$PREFIX/share/applications/net.3m4.MouseTouch.desktop" \
  /etc/udev/rules.d/60-mousetouch.rules /etc/modules-load.d/mousetouch.conf \
  /etc/systemd/user/mousetouch.service
sudo udevadm control --reload-rules
systemctl --user daemon-reload
[[ ${1:-} == --purge ]] && rm -rf "${XDG_CONFIG_HOME:-$HOME/.config}/mousetouch"
echo "removed"
