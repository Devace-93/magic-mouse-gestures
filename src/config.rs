//! Configuration model. Stored as TOML in
//! `$XDG_CONFIG_HOME/magic-mouse-gestures/config.toml`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub general: General,
    pub workspace_swipe: WorkspaceSwipe,
    pub taps: Taps,
    #[serde(rename = "gesture")]
    pub gestures: Vec<GestureRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    /// Substring that must appear in the evdev device name.
    pub device_match: String,
    /// Scroll-wheel events from the mouse driver are dropped while at least
    /// this many fingers touch the surface (prevents scrolling mid-gesture).
    pub suppress_scroll_from_fingers: u8,
}

impl Default for General {
    fn default() -> Self {
        Self {
            device_match: "Magic Mouse".into(),
            suppress_scroll_from_fingers: 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// Content follows the fingers (macOS style): swipe left -> next workspace.
    Natural,
    /// Swipe left -> previous workspace.
    Inverted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Axis {
    Horizontal,
    Vertical,
}

/// Continuous, compositor-animated workspace switching. Implemented by
/// replaying the finger motion on a virtual touchpad with `synthetic_fingers`
/// contacts, so the desktop's native swipe gesture handles animation,
/// velocity, pausing and dragging back.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceSwipe {
    pub enabled: bool,
    /// Real fingers on the mouse that trigger the swipe.
    pub fingers: u8,
    pub direction: Direction,
    /// Finger travel multiplier. Higher = less travel needed per workspace.
    pub sensitivity: f64,
    /// Contacts to synthesise on the virtual touchpad (4 = COSMIC, 3 = GNOME).
    pub synthetic_fingers: u8,
    /// Millimetres the fingers must travel before a swipe starts.
    pub activation_mm: f64,
    /// Axis the compositor uses for workspace swipes.
    pub axis: Axis,
    /// Whether the compositor's touchpad "natural scrolling" is on. `None`
    /// means auto-detect (COSMIC / GNOME) and fall back to `false`.
    pub compositor_natural_scroll: Option<bool>,
}

impl Default for WorkspaceSwipe {
    fn default() -> Self {
        Self {
            enabled: true,
            fingers: 2,
            direction: Direction::Natural,
            sensitivity: 0.4,
            synthetic_fingers: 4,
            activation_mm: 2.0,
            axis: Axis::Horizontal,
            compositor_natural_scroll: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Taps {
    /// Max duration of a touch to count as a tap.
    pub tap_max_ms: u64,
    /// Max finger travel during a tap.
    pub tap_max_mm: f64,
    /// Max gap between two taps to count as a double tap.
    pub double_tap_ms: u64,
}

impl Default for Taps {
    fn default() -> Self {
        Self {
            tap_max_ms: 300,
            tap_max_mm: 4.0,
            double_tap_ms: 400,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GestureKind {
    Swipe,
    Tap,
    DoubleTap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SwipeDir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// Key combination, e.g. "super+w", "ctrl+alt+right".
    Key { combo: String },
    /// Mouse button click: "left", "right" or "middle".
    Button { button: String },
    /// Shell command, run with `sh -c`.
    Command { command: String },
}

/// A discrete gesture: fires its action once.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GestureRule {
    pub kind: GestureKind,
    pub fingers: u8,
    /// Only for `swipe`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<SwipeDir>,
    pub action: Action,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            general: General::default(),
            workspace_swipe: WorkspaceSwipe::default(),
            taps: Taps::default(),
            gestures: vec![GestureRule {
                kind: GestureKind::DoubleTap,
                fingers: 2,
                direction: None,
                action: Action::Key {
                    combo: default_overview_combo(),
                },
                enabled: true,
            }],
        }
    }
}

/// Keyboard shortcut that shows the workspaces on the current desktop.
fn default_overview_combo() -> String {
    match std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        d if d.contains("cosmic") => "super+w".into(),
        d if d.contains("gnome") => "super".into(),
        d if d.contains("kde") => "super+w".into(),
        _ => "super+w".into(),
    }
}

pub fn default_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into());
            home.join(".config")
        });
    base.join("magic-mouse-gestures").join("config.toml")
}

pub fn load(path: &Path) -> Result<Config> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let cfg: Config = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(cfg)
}

/// Load the config, writing a commented default file if none exists.
pub fn load_or_create(path: &Path) -> Result<Config> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let cfg = Config::default();
        fs::write(path, to_toml(&cfg)?)?;
        log::info!("wrote default config to {}", path.display());
        return Ok(cfg);
    }
    load(path)
}

pub fn to_toml(cfg: &Config) -> Result<String> {
    let body = toml::to_string_pretty(cfg)?;
    Ok(format!(
        "# magic-mouse-gestures configuration\n\
         # Edit by hand; the daemon reloads it on save.\n\n{body}"
    ))
}

/// Resolve `compositor_natural_scroll` when it is set to auto.
pub fn detect_compositor_natural_scroll() -> bool {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_ascii_lowercase();
    if desktop.contains("cosmic") {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let candidates = [
            home.join(".config/cosmic/com.system76.CosmicComp/v1/input_touchpad"),
            PathBuf::from("/usr/share/cosmic/com.system76.CosmicComp/v1/input_touchpad"),
        ];
        for p in candidates {
            if let Ok(text) = fs::read_to_string(&p) {
                // RON: `natural_scroll: Some(true)` / `natural_scroll: None`
                let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
                if compact.contains("natural_scroll:Some(true)") {
                    return true;
                }
                if compact.contains("natural_scroll:Some(false)") || compact.contains("natural_scroll:None") {
                    return false;
                }
            }
        }
        return false;
    }
    if desktop.contains("gnome") {
        if let Ok(out) = std::process::Command::new("gsettings")
            .args(["get", "org.gnome.desktop.peripherals.touchpad", "natural-scroll"])
            .output()
        {
            return String::from_utf8_lossy(&out.stdout).trim() == "true";
        }
    }
    false
}
