//! Tracks the focused window's app id so gesture rules can be scoped to an
//! application (`app = "firefox"`).
//!
//! Two Wayland protocol families are supported:
//! - `zwlr_foreign_toplevel_manager_v1` (KDE, Sway, Hyprland, wlroots): app id
//!   and activated state come from one handle.
//! - `ext_foreign_toplevel_list_v1` + `zcosmic_toplevel_info_v1` (COSMIC): the
//!   ext handle carries the app id, the cosmic handle the activated state.
//!
//! GNOME exposes neither, so there `app` rules never match and generic rules
//! apply. Everything runs on its own thread; the result is just a shared
//! `Option<String>`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use wayland_client::backend::ObjectId;
use wayland_client::protocol::{wl_registry, wl_registry::WlRegistry};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};

use cosmic_protocols::toplevel_info::v1::client::{
    zcosmic_toplevel_handle_v1::{self, ZcosmicToplevelHandleV1},
    zcosmic_toplevel_info_v1::{self, ZcosmicToplevelInfoV1},
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
    ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
};

/// App id of the focused window, if known.
pub type Focus = Arc<Mutex<Option<String>>>;

/// `activated` entry in both the wlr and the cosmic `state` enums.
const STATE_ACTIVATED: u32 = 2;

#[derive(Default)]
struct Toplevel {
    app_id: String,
    active: bool,
}

struct State {
    focus: Focus,
    toplevels: HashMap<ObjectId, Toplevel>,
    cosmic_info: Option<ZcosmicToplevelInfoV1>,
    /// ext handles seen before zcosmic_toplevel_info_v1 was bound.
    pending_ext: Vec<ExtForeignToplevelHandleV1>,
    wlr_bound: bool,
}

impl State {
    fn update(&mut self, id: &ObjectId, f: impl FnOnce(&mut Toplevel)) {
        let t = self.toplevels.entry(id.clone()).or_default();
        f(t);
        if t.active && !t.app_id.is_empty() {
            let app = t.app_id.clone();
            let mut focus = self.focus.lock().unwrap();
            if focus.as_deref() != Some(app.as_str()) {
                log::debug!("focused app: {app}");
                *focus = Some(app);
            }
        }
    }

    fn set_state(&mut self, id: &ObjectId, raw: &[u8]) {
        let active = raw
            .chunks_exact(4)
            .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
            .any(|v| v == STATE_ACTIVATED);
        self.update(id, |t| t.active = active);
    }

    fn attach_cosmic(&mut self, ext: &ExtForeignToplevelHandleV1, qh: &QueueHandle<Self>) {
        if let Some(info) = &self.cosmic_info {
            info.get_cosmic_toplevel(ext, qh, ext.id());
        } else {
            self.pending_ext.push(ext.clone());
        }
    }
}

/// Start tracking in a background thread. Never fails: without a usable
/// Wayland connection or protocol the focus simply stays `None`.
pub fn spawn() -> Focus {
    let focus: Focus = Arc::new(Mutex::new(None));
    let shared = Arc::clone(&focus);
    std::thread::Builder::new()
        .name("focus-tracker".into())
        .spawn(move || {
            if let Err(e) = run(shared) {
                log::info!("per-app rules unavailable: {e}");
            }
        })
        .expect("spawn focus tracker");
    focus
}

fn run(focus: Focus) -> anyhow::Result<()> {
    let conn = Connection::connect_to_env()?;
    let display = conn.display();
    let mut queue = conn.new_event_queue::<State>();
    let qh = queue.handle();
    display.get_registry(&qh, ());
    let mut state = State {
        focus,
        toplevels: HashMap::new(),
        cosmic_info: None,
        pending_ext: Vec::new(),
        wlr_bound: false,
    };
    queue.roundtrip(&mut state)?;
    if !state.wlr_bound && state.cosmic_info.is_none() {
        anyhow::bail!("compositor exposes neither wlr-foreign-toplevel-management nor cosmic-toplevel-info");
    }
    log::info!(
        "focus tracking via {}",
        if state.wlr_bound { "wlr-foreign-toplevel-management" } else { "ext-foreign-toplevel-list + cosmic-toplevel-info" }
    );
    loop {
        queue.blocking_dispatch(&mut state)?;
    }
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(state: &mut Self, registry: &WlRegistry, event: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        let wl_registry::Event::Global { name, interface, version } = event else { return };
        match interface.as_str() {
            "zwlr_foreign_toplevel_manager_v1" => {
                registry.bind::<ZwlrForeignToplevelManagerV1, _, _>(name, version.min(3), qh, ());
                state.wlr_bound = true;
            }
            "ext_foreign_toplevel_list_v1" => {
                registry.bind::<ExtForeignToplevelListV1, _, _>(name, 1, qh, ());
            }
            "zcosmic_toplevel_info_v1" if version >= 2 => {
                let info = registry.bind::<ZcosmicToplevelInfoV1, _, _>(name, version.min(2), qh, ());
                state.cosmic_info = Some(info);
                for ext in std::mem::take(&mut state.pending_ext) {
                    state.attach_cosmic(&ext, qh);
                }
            }
            _ => {}
        }
    }
}

// ---- wlr -----------------------------------------------------------------

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for State {
    fn event(_: &mut Self, _: &ZwlrForeignToplevelManagerV1, _: zwlr_foreign_toplevel_manager_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        // `toplevel` creates the handle object; its events arrive below.
    }
    wayland_client::event_created_child!(State, ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for State {
    fn event(state: &mut Self, h: &ZwlrForeignToplevelHandleV1, event: zwlr_foreign_toplevel_handle_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        use zwlr_foreign_toplevel_handle_v1::Event;
        match event {
            Event::AppId { app_id } => state.update(&h.id(), |t| t.app_id = app_id),
            Event::State { state: raw } => state.set_state(&h.id(), &raw),
            Event::Closed => {
                state.toplevels.remove(&h.id());
                h.destroy();
            }
            _ => {}
        }
    }
}

// ---- ext + cosmic ----------------------------------------------------------

impl Dispatch<ExtForeignToplevelListV1, ()> for State {
    fn event(_: &mut Self, _: &ExtForeignToplevelListV1, _: ext_foreign_toplevel_list_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    wayland_client::event_created_child!(State, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for State {
    fn event(state: &mut Self, h: &ExtForeignToplevelHandleV1, event: ext_foreign_toplevel_handle_v1::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        use ext_foreign_toplevel_handle_v1::Event;
        match event {
            Event::AppId { app_id } => {
                let first = !state.toplevels.contains_key(&h.id());
                state.update(&h.id(), |t| t.app_id = app_id);
                if first {
                    state.attach_cosmic(h, qh);
                }
            }
            Event::Closed => {
                state.toplevels.remove(&h.id());
                h.destroy();
            }
            _ => {}
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
        match event {
            Event::State { state: raw } => state.set_state(ext_id, &raw),
            Event::Closed => h.destroy(),
            _ => {}
        }
    }
}
