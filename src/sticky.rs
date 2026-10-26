//! `mousetouch sticky APP_ID [TITLE] on|off`: ask the compositor to show a
//! window on every workspace, so it stays put while workspaces slide.
//!
//! Wayland has no core protocol for this; COSMIC exposes it through
//! `zcosmic_toplevel_manager_v1.set_sticky` (version 3+), which is what this
//! uses. The settings app calls it for its live-view window. On other
//! desktops the command reports that it is unsupported and the user can use
//! the window menu instead (GNOME: "Always on Visible Workspace", KDE: "All
//! Desktops").

use anyhow::Result;
use std::collections::HashMap;
use wayland_client::backend::ObjectId;
use wayland_client::protocol::{wl_registry, wl_registry::WlRegistry};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};

use cosmic_protocols::toplevel_info::v1::client::{
    zcosmic_toplevel_handle_v1::{self, ZcosmicToplevelHandleV1},
    zcosmic_toplevel_info_v1::{self, ZcosmicToplevelInfoV1},
};
use cosmic_protocols::toplevel_management::v1::client::zcosmic_toplevel_manager_v1::{self, ZcosmicToplevelManagerV1};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
    ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
};

/// `sticky` entry of `zcosmic_toplevel_manager_v1.zcosmic_toplevel_manager_capabilities`.
const CAP_STICKY: u32 = 7;
/// `sticky` entry of `zcosmic_toplevel_handle_v1.state`.
const STATE_STICKY: u32 = 4;

#[derive(Default)]
struct Toplevel {
    app_id: String,
    title: String,
    cosmic: Option<ZcosmicToplevelHandleV1>,
    sticky: bool,
}

struct State {
    toplevels: HashMap<ObjectId, Toplevel>,
    info: Option<ZcosmicToplevelInfoV1>,
    manager: Option<ZcosmicToplevelManagerV1>,
    manager_version: u32,
    capabilities: Vec<u32>,
    pending_ext: Vec<ExtForeignToplevelHandleV1>,
}

impl State {
    /// Ask for the cosmic handle of an ext toplevel. The handle is usable as
    /// soon as it is created; cosmic-comp does not send its initial state
    /// (only changes), so the request is what we keep, not an event.
    fn attach_cosmic(&mut self, ext: &ExtForeignToplevelHandleV1, qh: &QueueHandle<Self>) {
        if let Some(info) = &self.info {
            let handle = info.get_cosmic_toplevel(ext, qh, ext.id());
            if let Some(t) = self.toplevels.get_mut(&ext.id()) {
                t.cosmic = Some(handle);
            }
        } else {
            self.pending_ext.push(ext.clone());
        }
    }
}

pub enum Outcome {
    /// The compositor offers no way to do this (not COSMIC, or too old).
    Unsupported(&'static str),
    /// Number of windows the request was sent for.
    Changed(usize),
}

/// `sticky list`: print every window the compositor reports.
pub fn list() -> Result<()> {
    let (_queue, state) = connect()?;
    let mut rows: Vec<&Toplevel> = state.toplevels.values().collect();
    rows.sort_by(|a, b| (&a.app_id, &a.title).cmp(&(&b.app_id, &b.title)));
    for t in rows {
        println!("{:<40} sticky={:<5} \"{}\"", t.app_id, t.sticky, t.title);
    }
    match &state.manager {
        None => println!("(no zcosmic_toplevel_manager_v1: sticky requests unsupported here)"),
        Some(_) => println!(
            "(zcosmic_toplevel_manager_v1 version {}, capabilities {:?}; sticky needs version 3 and capability {CAP_STICKY})",
            state.manager_version, state.capabilities
        ),
    }
    Ok(())
}

/// Connect and collect the toplevels: 1 roundtrip for globals, 2 for the
/// toplevel handles and their properties, 3 for the cosmic handles requested
/// for them (and the manager's capabilities).
fn connect() -> Result<(wayland_client::EventQueue<State>, State)> {
    let conn = Connection::connect_to_env()?;
    let display = conn.display();
    let mut queue = conn.new_event_queue::<State>();
    let qh = queue.handle();
    display.get_registry(&qh, ());
    let mut state = State {
        toplevels: HashMap::new(),
        info: None,
        manager: None,
        manager_version: 0,
        capabilities: Vec::new(),
        pending_ext: Vec::new(),
    };
    for _ in 0..3 {
        queue.roundtrip(&mut state)?;
    }
    Ok((queue, state))
}

/// Find the windows with this app id (and title, if given) and make them
/// sticky or not.
pub fn set_sticky(app_id: &str, title: Option<&str>, on: bool) -> Result<Outcome> {
    let (mut queue, mut state) = connect()?;
    let Some(manager) = state.manager.clone() else {
        return Ok(Outcome::Unsupported("the compositor does not expose zcosmic_toplevel_manager_v1 (sticky windows need COSMIC)"));
    };
    if state.manager_version < 3 {
        return Ok(Outcome::Unsupported("the compositor's toplevel manager is too old for sticky windows (needs version 3)"));
    }
    if !state.capabilities.contains(&CAP_STICKY) {
        // Not every cosmic-comp that handles the request advertises the
        // capability; send it anyway and judge by the state that comes back.
        log::debug!("sticky capability not advertised (have {:?}); trying anyway", state.capabilities);
    }
    if state.info.is_none() {
        return Ok(Outcome::Unsupported("the compositor does not expose zcosmic_toplevel_info_v1 (version 2+)"));
    }
    let targets: Vec<(ObjectId, ZcosmicToplevelHandleV1)> = state
        .toplevels
        .iter()
        .filter(|(_, t)| t.app_id == app_id && title.map_or(true, |w| t.title == w))
        .filter_map(|(id, t)| t.cosmic.clone().map(|c| (id.clone(), c)))
        .collect();
    for (_, handle) in &targets {
        if on {
            manager.set_sticky(handle);
        } else {
            manager.unset_sticky(handle);
        }
    }
    queue.roundtrip(&mut state)?;
    let mut honoured = 0;
    for (id, _) in &targets {
        if let Some(t) = state.toplevels.get(id) {
            println!("{} \"{}\": sticky={}", t.app_id, t.title, t.sticky);
            if t.sticky == on {
                honoured += 1;
            }
        }
    }
    if !targets.is_empty() && honoured == 0 {
        return Ok(Outcome::Unsupported("the compositor ignored the sticky request (sticky windows need COSMIC)"));
    }
    Ok(Outcome::Changed(targets.len()))
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(state: &mut Self, registry: &WlRegistry, event: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        let wl_registry::Event::Global { name, interface, version } = event else { return };
        match interface.as_str() {
            "ext_foreign_toplevel_list_v1" => {
                registry.bind::<ExtForeignToplevelListV1, _, _>(name, 1, qh, ());
            }
            "zcosmic_toplevel_info_v1" if version >= 2 => {
                let info = registry.bind::<ZcosmicToplevelInfoV1, _, _>(name, version.min(2), qh, ());
                state.info = Some(info);
                for ext in std::mem::take(&mut state.pending_ext) {
                    state.attach_cosmic(&ext, qh);
                }
            }
            "zcosmic_toplevel_manager_v1" => {
                let v = version.min(4);
                state.manager = Some(registry.bind::<ZcosmicToplevelManagerV1, _, _>(name, v, qh, ()));
                state.manager_version = v;
            }
            _ => {}
        }
    }
}

impl Dispatch<ZcosmicToplevelManagerV1, ()> for State {
    fn event(state: &mut Self, _: &ZcosmicToplevelManagerV1, event: zcosmic_toplevel_manager_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let zcosmic_toplevel_manager_v1::Event::Capabilities { capabilities } = event {
            state.capabilities = capabilities.chunks_exact(4).map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]])).collect();
        }
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for State {
    fn event(_: &mut Self, _: &ExtForeignToplevelListV1, _: ext_foreign_toplevel_list_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    wayland_client::event_created_child!(State, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for State {
    fn event(state: &mut Self, h: &ExtForeignToplevelHandleV1, event: ext_foreign_toplevel_handle_v1::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        use ext_foreign_toplevel_handle_v1::Event;
        let first = !state.toplevels.contains_key(&h.id());
        let t = state.toplevels.entry(h.id()).or_default();
        match event {
            Event::AppId { app_id } => t.app_id = app_id,
            Event::Title { title } => t.title = title,
            Event::Closed => {
                state.toplevels.remove(&h.id());
                h.destroy();
                return;
            }
            _ => {}
        }
        if first {
            state.attach_cosmic(h, qh);
        }
    }
}

impl Dispatch<ZcosmicToplevelInfoV1, ()> for State {
    fn event(_: &mut Self, _: &ZcosmicToplevelInfoV1, _: zcosmic_toplevel_info_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    wayland_client::event_created_child!(State, ZcosmicToplevelInfoV1, [
        zcosmic_toplevel_info_v1::EVT_TOPLEVEL_OPCODE => (ZcosmicToplevelHandleV1, ObjectId::null()),
    ]);
}

/// User data = id of the ext handle this cosmic handle describes.
impl Dispatch<ZcosmicToplevelHandleV1, ObjectId> for State {
    fn event(state: &mut Self, h: &ZcosmicToplevelHandleV1, event: zcosmic_toplevel_handle_v1::Event, ext_id: &ObjectId, _: &Connection, _: &QueueHandle<Self>) {
        use zcosmic_toplevel_handle_v1::Event;
        let Some(t) = state.toplevels.get_mut(ext_id) else { return };
        match event {
            Event::State { state: raw } => {
                t.sticky = raw.chunks_exact(4).map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]])).any(|v| v == STATE_STICKY);
            }
            Event::Closed => {
                t.cosmic = None;
                h.destroy();
            }
            _ => {}
        }
    }
}
