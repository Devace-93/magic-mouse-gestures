//! Bluetooth pairing repair for the Magic Mouse.
//!
//! When the mouse is paired with a Mac and comes back, BlueZ is often left
//! with a stale record (`Paired: yes`, `Bonded: no`): the mouse holds the
//! key it made with the Mac, bluetoothd rejects its HID connection as
//! "!bonded", and no input device ever appears. Forgetting the mouse in the
//! desktop's Bluetooth panel does not always clear that record. The fix is
//! always the same: remove the device, let it advertise again (switch it off
//! and on), pair, trust, connect. `mousetouch bt-check` reports the state and
//! `mousetouch bt-repair` runs the fix; the settings app calls both.
//!
//! Everything goes through `bluetoothctl` (shipped with BlueZ), which also
//! supplies the pairing agent.

use anyhow::{anyhow, bail, Context, Result};
use std::process::Command;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default)]
pub struct BtDevice {
    pub addr: String,
    pub name: String,
    pub paired: bool,
    pub bonded: bool,
    pub trusted: bool,
    pub connected: bool,
}

impl BtDevice {
    /// Paired without a bond: bluetoothd will reject the mouse's connection.
    pub fn stale(&self) -> bool {
        self.paired && !self.bonded
    }
}

/// Run bluetoothctl non-interactively and return its output without the
/// ANSI colour codes it prints even when not on a terminal.
fn bluetoothctl(args: &[&str]) -> Result<String> {
    let out = Command::new("bluetoothctl")
        .args(args)
        .output()
        .context("running bluetoothctl (is BlueZ installed?)")?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok(strip_ansi(&text))
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // ESC [ ... final byte in 0x40..=0x7e
            if chars.peek() == Some(&'[') {
                chars.next();
                for d in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&d) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// `bluetoothctl devices` -> (address, name).
pub fn devices() -> Result<Vec<(String, String)>> {
    let text = bluetoothctl(&["devices"])?;
    Ok(text
        .lines()
        .filter_map(|l| {
            let rest = l.trim().strip_prefix("Device ")?;
            let (addr, name) = rest.split_once(' ').unwrap_or((rest, ""));
            Some((addr.to_string(), name.trim().to_string()))
        })
        .collect())
}

pub fn info(addr: &str) -> Result<BtDevice> {
    let text = bluetoothctl(&["info", addr])?;
    if text.contains("not available") {
        bail!("device {addr} not known to BlueZ");
    }
    let mut d = BtDevice { addr: addr.to_string(), ..Default::default() };
    for line in text.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once(':') else { continue };
        let value = value.trim();
        match key.trim() {
            "Name" => d.name = value.to_string(),
            "Paired" => d.paired = value == "yes",
            "Bonded" => d.bonded = value == "yes",
            "Trusted" => d.trusted = value == "yes",
            "Connected" => d.connected = value == "yes",
            _ => {}
        }
    }
    Ok(d)
}

/// Known devices whose name contains `name_match` (case-insensitive).
pub fn find(name_match: &str) -> Result<Vec<BtDevice>> {
    let needle = name_match.to_ascii_lowercase();
    let mut out = Vec::new();
    for (addr, name) in devices()? {
        if name.to_ascii_lowercase().contains(&needle) {
            out.push(info(&addr)?);
        }
    }
    Ok(out)
}

/// `bt-check NAME`: one line per matching device. Exit status: 0 = paired
/// and bonded, 3 = stale pairing (needs `bt-repair`), 4 = not paired or
/// not known.
pub fn check(name_match: &str) -> Result<()> {
    let devs = find(name_match)?;
    if devs.is_empty() {
        println!("none: no Bluetooth device named like \"{name_match}\"");
        std::process::exit(4);
    }
    let mut status = 0;
    for d in &devs {
        let state = if d.stale() {
            status = status.max(3);
            "stale"
        } else if d.paired && d.bonded {
            "ok"
        } else {
            status = status.max(4);
            "unpaired"
        };
        println!(
            "{state}: {} {} paired={} bonded={} trusted={} connected={}",
            d.addr, d.name, d.paired, d.bonded, d.trusted, d.connected
        );
    }
    std::process::exit(status);
}

/// `bt-repair NAME [TIMEOUT_S]`: remove every matching device, wait for the
/// mouse to advertise again, then pair, trust and connect. Progress goes to
/// stdout one line at a time (`step:`, `hint:`, `ok:`), which the settings
/// app shows as it happens.
pub fn repair(name_match: &str, timeout: Duration) -> Result<()> {
    let known = find(name_match)?;
    for d in &known {
        println!("step: removing {} ({})", d.name, d.addr);
        let out = bluetoothctl(&["remove", &d.addr])?;
        if !out.contains("removed") {
            bail!("could not remove {}: {}", d.addr, out.trim());
        }
    }
    if known.is_empty() {
        println!("step: nothing to remove");
    }

    println!("hint: switch the mouse off and on so it advertises again");
    let start = Instant::now();
    let addr = loop {
        if start.elapsed() > timeout {
            bail!("the mouse did not show up within {} s; switch it off and on and run again", timeout.as_secs());
        }
        println!("step: scanning for \"{name_match}\" ...");
        bluetoothctl(&["--timeout", "6", "scan", "on"])?;
        let needle = name_match.to_ascii_lowercase();
        if let Some((addr, name)) = devices()?.into_iter().find(|(_, n)| n.to_ascii_lowercase().contains(&needle)) {
            println!("step: found {name} ({addr})");
            break addr;
        }
    };

    let mut last_err = String::new();
    for attempt in 1..=3 {
        println!("step: pairing (attempt {attempt})");
        let out = bluetoothctl(&["pair", &addr])?;
        if out.contains("Pairing successful") || out.contains("Already paired") {
            last_err.clear();
            break;
        }
        last_err = out.lines().filter(|l| l.contains("Failed") || l.contains("Error")).last().unwrap_or("pairing failed").trim().to_string();
        std::thread::sleep(Duration::from_secs(2));
    }
    if !last_err.is_empty() {
        bail!("{last_err}");
    }
    println!("step: trusting");
    bluetoothctl(&["trust", &addr])?;
    println!("step: connecting");
    let out = bluetoothctl(&["connect", &addr])?;
    if !out.contains("Connection successful") {
        // The mouse often connects by itself right after pairing.
        std::thread::sleep(Duration::from_secs(2));
    }
    let d = info(&addr)?;
    if !d.bonded {
        return Err(anyhow!("paired but still not bonded; remove the mouse on the other computer too and try again"));
    }
    println!("ok: {} paired={} bonded={} trusted={} connected={}", d.name, d.paired, d.bonded, d.trusted, d.connected);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_colour_codes() {
        assert_eq!(strip_ansi("[\x1b[0;93mCHG\x1b[0m] Device AA Paired: yes"), "[CHG] Device AA Paired: yes");
    }
}
