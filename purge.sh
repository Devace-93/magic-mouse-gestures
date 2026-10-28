#!/usr/bin/env bash
# Remove every trace of mousetouch from this machine, whichever way it was
# installed (install.sh, get.sh tarball, .deb, .rpm, Homebrew, cargo install),
# so a clean install can be tested again. Also clears what the old
# magic-mouse-gestures name left behind.
#
#   ./purge.sh                 remove binaries, services, rules, menu entries,
#                              runtime state AND ~/.config/mousetouch
#   ./purge.sh --keep-config   same, but keep your settings
#   ./purge.sh --untap         also remove the Homebrew tap
#   ./purge.sh --dry-run       only print what would be removed
#
# Needs sudo for the system paths; everything under $HOME is done as you.
set -uo pipefail

KEEP_CONFIG=0; UNTAP=0; DRY=0
for a in "$@"; do
  case $a in
    --keep-config) KEEP_CONFIG=1 ;;
    --untap) UNTAP=1 ;;
    --dry-run) DRY=1 ;;
    -h|--help) sed -n 2,13p "$0"; exit 0 ;;
    *) echo "unknown option: $a" >&2; exit 2 ;;
  esac
done

have() { command -v "$1" >/dev/null 2>&1; }
say() { printf '>> %s\n' "$*"; }
run() { if [[ $DRY == 1 ]]; then printf '   would run: %s\n' "$*"; else "$@"; fi; }
sudo_rm() {
  local existing=()
  for f in "$@"; do [[ -e $f || -L $f ]] && existing+=("$f"); done
  [[ ${#existing[@]} -eq 0 ]] && return 0
  say "removing (sudo): ${existing[*]}"
  run sudo rm -rf "${existing[@]}"
}
user_rm() {
  local existing=()
  for f in "$@"; do [[ -e $f || -L $f ]] && existing+=("$f"); done
  [[ ${#existing[@]} -eq 0 ]] && return 0
  say "removing: ${existing[*]}"
  run rm -rf "${existing[@]}"
}

CONFIG_HOME=${XDG_CONFIG_HOME:-$HOME/.config}
DATA_HOME=${XDG_DATA_HOME:-$HOME/.local/share}
RUNTIME_DIR=${XDG_RUNTIME_DIR:-/tmp}
ID=io.github.Devace_93.MouseTouch

# 1. Stop everything first, so grabbed devices are released.
say "stopping processes and user services"
for unit in mousetouch.service sh.brew.mousetouch.service homebrew.mousetouch.service magic-mouse-gestures.service; do
  if systemctl --user list-unit-files "$unit" --no-legend 2>/dev/null | grep -q "$unit"; then
    run systemctl --user disable --now "$unit" 2>/dev/null || true
  fi
done
if have brew && brew list --formula 2>/dev/null | grep -qx mousetouch; then
  run brew services stop mousetouch 2>/dev/null || true
fi
run pkill -x mousetouch 2>/dev/null || true
run pkill -x magic-mouse-gestures 2>/dev/null || true
run pkill -f 'mousetouch-settings$' 2>/dev/null || true
run pkill -f 'magic-mouse-gestures-settings$' 2>/dev/null || true

# 2. Package managers.
if have dpkg && dpkg -s mousetouch >/dev/null 2>&1; then
  say "removing the .deb (apt)"
  run sudo apt-get remove --purge -y mousetouch
fi
if have rpm && rpm -q mousetouch >/dev/null 2>&1; then
  say "removing the .rpm"
  if have dnf; then run sudo dnf remove -y mousetouch; else run sudo zypper --non-interactive remove mousetouch; fi
fi
if have brew && brew list --formula 2>/dev/null | grep -qx mousetouch; then
  say "removing the Homebrew formula"
  run brew uninstall --force mousetouch
fi
if [[ $UNTAP == 1 ]] && have brew && brew tap 2>/dev/null | grep -qi '^devace-93/tap$'; then
  say "removing the Homebrew tap"
  run brew untap devace-93/tap
fi
if have cargo && [[ -x $HOME/.cargo/bin/mousetouch ]]; then
  say "removing the cargo install"
  run cargo uninstall mousetouch
fi

# 3. Files from install.sh / get.sh tarball (any prefix) and package leftovers.
for prefix in /usr/local /usr /opt/mousetouch; do
  sudo_rm "$prefix/bin/mousetouch" "$prefix/bin/mousetouch-settings" \
    "$prefix/bin/magic-mouse-gestures" "$prefix/bin/magic-mouse-gestures-settings" \
    "$prefix/share/applications/$ID.desktop" "$prefix/share/applications/org.magicmouse.Gestures.desktop" \
    "$prefix/share/icons/hicolor/scalable/apps/$ID.svg" \
    "$prefix/share/mousetouch" "$prefix/share/doc/mousetouch" \
    "$prefix/lib/udev/rules.d/60-mousetouch.rules" "$prefix/lib/modules-load.d/mousetouch.conf" \
    "$prefix/lib/systemd/user/mousetouch.service"
done
sudo_rm /etc/udev/rules.d/60-mousetouch.rules /etc/udev/rules.d/60-magic-mouse-gestures.rules \
  /etc/modules-load.d/mousetouch.conf /etc/modules-load.d/magic-mouse-gestures.conf \
  /etc/systemd/user/mousetouch.service /etc/systemd/user/magic-mouse-gestures.service

# 4. Per-user state: units brew services wrote, menu entry the daemon or the
#    caveats created, icon link, runtime socket, caches.
user_rm "$CONFIG_HOME/systemd/user/sh.brew.mousetouch.service" "$CONFIG_HOME/systemd/user/homebrew.mousetouch.service" \
  "$CONFIG_HOME"/systemd/user/*.wants/mousetouch.service "$CONFIG_HOME"/systemd/user/*.wants/sh.brew.mousetouch.service \
  "$CONFIG_HOME"/systemd/user/*.wants/magic-mouse-gestures.service \
  "$DATA_HOME/applications/$ID.desktop" "$DATA_HOME/applications/org.magicmouse.Gestures.desktop" \
  "$DATA_HOME/icons/hicolor/scalable/apps/$ID.svg" \
  "$RUNTIME_DIR/mousetouch.sock" "/tmp/mousetouch-$(id -u).sock" \
  "$HOME/.cache/mousetouch"
if [[ $KEEP_CONFIG == 1 ]]; then
  say "keeping $CONFIG_HOME/mousetouch"
else
  user_rm "$CONFIG_HOME/mousetouch" "$CONFIG_HOME/magic-mouse-gestures"
fi

# 5. Reload what read those files.
if [[ $DRY == 0 ]]; then
  systemctl --user daemon-reload 2>/dev/null || true
  systemctl --user reset-failed 2>/dev/null || true
  if have udevadm; then sudo udevadm control --reload-rules 2>/dev/null || true; fi
  have update-desktop-database && update-desktop-database "$DATA_HOME/applications" 2>/dev/null || true
fi

# 6. Show what is left, if anything.
say "check"
left=0
for f in $(command -v mousetouch mousetouch-settings magic-mouse-gestures 2>/dev/null); do echo "   still in PATH: $f"; left=1; done
for unit in mousetouch.service sh.brew.mousetouch.service homebrew.mousetouch.service magic-mouse-gestures.service; do
  if systemctl --user list-unit-files "$unit" --no-legend 2>/dev/null | grep -q "$unit"; then echo "   unit still known: $unit"; left=1; fi
done
for f in /etc/udev/rules.d/60-mousetouch.rules /etc/modules-load.d/mousetouch.conf "$DATA_HOME/applications/$ID.desktop"; do
  [[ -e $f ]] && { echo "   still present: $f"; left=1; }
done
if pgrep -x mousetouch >/dev/null 2>&1; then echo "   daemon still running"; left=1; fi
if [[ $left == 0 ]]; then
  echo "   nothing left$( [[ $KEEP_CONFIG == 1 ]] && echo " (config kept)" )"
else
  echo "   some pieces remain (see above)"
fi
[[ $DRY == 1 ]] && echo "(dry run: nothing was changed)"
exit 0
