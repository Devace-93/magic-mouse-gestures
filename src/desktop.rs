//! App-menu entry for the settings app when the daemon lives somewhere the
//! desktop does not scan (Homebrew: `/home/linuxbrew/.linuxbrew/opt/...`).
//!
//! Launchers build the menu from `XDG_DATA_DIRS` (by default
//! `/usr/local/share:/usr/share`) and `~/.local/share`. A `.deb`, `.rpm` or
//! `install.sh` install lands in one of those, so its desktop file is found.
//! A Homebrew install is not, and the launcher's PATH lacks Homebrew's bin
//! too. The daemon runs as the user, so on start it writes a user entry with
//! absolute `Exec=` and `Icon=` paths, once, and keeps it current.

use std::path::{Path, PathBuf};

const DESKTOP_ID: &str = "io.github.Devace_93.MouseTouch";

/// Install prefix of the running binary (`<prefix>/bin/mousetouch`), if the
/// layout is the expected one.
fn install_prefix() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let bin = exe.parent()?;
    if bin.file_name()? != "bin" {
        return None;
    }
    bin.parent().map(Path::to_path_buf)
}

/// Directories the desktop scans for applications, from `XDG_DATA_DIRS`
/// (or the spec's default) plus the user's own data dir.
fn data_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = match std::env::var_os("XDG_DATA_DIRS") {
        Some(v) if !v.is_empty() => std::env::split_paths(&v).collect(),
        _ => vec![PathBuf::from("/usr/local/share"), PathBuf::from("/usr/share")],
    };
    dirs.push(user_data_dir());
    dirs
}

fn user_data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into());
            home.join(".local/share")
        })
}

/// The shipped desktop file with `Exec=` and `Icon=` made absolute.
fn render_entry(template: &str, prefix: &Path) -> String {
    let exec = prefix.join("bin/mousetouch-settings");
    let icon = prefix.join(format!("share/icons/hicolor/scalable/apps/{DESKTOP_ID}.svg"));
    let mut out = String::with_capacity(template.len() + 128);
    for line in template.lines() {
        if line.starts_with("Exec=") {
            out.push_str(&format!("Exec={}", exec.display()));
        } else if line.starts_with("Icon=") {
            out.push_str(&format!("Icon={}", icon.display()));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// Write `~/.local/share/applications/<id>.desktop` when the daemon's own
/// `share` is not on the desktop's search path. Idempotent; never fails the
/// daemon, only logs.
pub fn ensure_app_menu_entry() {
    let Some(prefix) = install_prefix() else { return };
    let share = prefix.join("share");
    let shipped = share.join("applications").join(format!("{DESKTOP_ID}.desktop"));
    if !shipped.is_file() {
        return;
    }
    let scanned = data_dirs().iter().any(|d| same_dir(d, &share));
    if scanned {
        return;
    }
    let target = user_data_dir().join("applications").join(format!("{DESKTOP_ID}.desktop"));
    let template = match std::fs::read_to_string(&shipped) {
        Ok(t) => t,
        Err(e) => {
            log::debug!("app-menu entry: cannot read {}: {e}", shipped.display());
            return;
        }
    };
    let wanted = render_entry(&template, &prefix);
    if std::fs::read_to_string(&target).map_or(false, |cur| cur == wanted) {
        return;
    }
    let result = target.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::write(&target, &wanted));
    match result {
        Ok(()) => log::info!("app-menu entry for the settings app written to {} ({} is not on XDG_DATA_DIRS)", target.display(), share.display()),
        Err(e) => log::warn!("app-menu entry: cannot write {}: {e}", target.display()),
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_and_icon_become_absolute() {
        let tpl = "[Desktop Entry]\nType=Application\nName=MouseTouch\nExec=mousetouch-settings\nIcon=io.github.Devace_93.MouseTouch\nTerminal=false\n";
        let out = render_entry(tpl, Path::new("/home/linuxbrew/.linuxbrew/opt/mousetouch"));
        assert!(out.contains("Exec=/home/linuxbrew/.linuxbrew/opt/mousetouch/bin/mousetouch-settings\n"));
        assert!(out.contains("Icon=/home/linuxbrew/.linuxbrew/opt/mousetouch/share/icons/hicolor/scalable/apps/io.github.Devace_93.MouseTouch.svg\n"));
        assert!(out.contains("Name=MouseTouch\n"));
        assert_eq!(out.lines().count(), 6);
    }
}
