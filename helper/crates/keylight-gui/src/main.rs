//! keylight-gui — LimeLight desktop GUI (Slint).
//! Talks to keylightd over its localhost API; see docs/API.md.

mod api;
mod autostart;
mod daemon;
mod fetch;
mod update_queue;

use api::ApiClient;
use fetch::Fetcher;
use i_slint_backend_winit::{EventResult, WinitWindowAccessor};
use limelight_core::api::{
    Group, LightRecord, LightStateResponse, Preset, Settings, UpdateRequest,
};
use limelight_core::convert::{
    brightness_to_slider, kelvin_to_warmth, slider_to_brightness, warmth_to_kelvin,
};
use slint::{Model, ModelRc, SharedString, VecModel};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use update_queue::{UpdateCommand, UpdateTarget};
use winit::event::WindowEvent;

slint::include_modules!();

/// Poll the daemon for light state this often while the window is focused.
const POLL_INTERVAL: Duration = Duration::from_secs(10);
/// After the user touched a control, ignore polled values for this long so
/// a slow light does not snap the slider back.
const USER_CHANGE_GRACE: Duration = Duration::from_millis(1500);

/// Everything the UI thread needs to reconcile models. Lives in an `Rc`.
struct Ctx {
    ui: slint::Weak<MainWindow>,
    fetcher: Fetcher,
    cmd_tx: mpsc::Sender<UpdateCommand>,
    states: RefCell<Vec<LightStateResponse>>,
    groups: RefCell<Vec<Group>>,
    presets: RefCell<Vec<Preset>>,
    /// "All Lights" slider positions (brightness, warmth). A master control:
    /// set by the user, initialised from the average once, never overwritten
    /// by member changes.
    all_sliders: Cell<Option<(f32, f32)>>,
    /// Same for each group, keyed by group name.
    group_sliders: RefCell<HashMap<String, (f32, f32)>>,
    dragging: Cell<bool>,
    last_user_change: Cell<Instant>,
    last_snapshot_request: Cell<Instant>,
    /// Last time we tried to (re)start the daemon after finding it gone.
    last_daemon_attempt: Cell<Instant>,
    window_focused: Cell<bool>,
}

impl Ctx {
    fn ui(&self) -> Option<MainWindow> {
        self.ui.upgrade()
    }

    fn touched(&self) {
        self.last_user_change.set(Instant::now());
    }

    fn in_grace(&self) -> bool {
        self.dragging.get() || self.last_user_change.get().elapsed() < USER_CHANGE_GRACE
    }

    /// Fetch states + groups and refresh both tabs. Debounced to 1/s.
    fn request_snapshot(self: &Rc<Self>) {
        if self.last_snapshot_request.get().elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last_snapshot_request.set(Instant::now());
        let ctx = Rc::clone(self);
        self.fetcher.run(
            |api| {
                let states = api.get_states();
                let groups = api.get_groups();
                let presets = api.get_presets();
                (states, groups, presets)
            },
            move |(states, groups, presets)| {
                let online = states.is_ok();
                if let Some(ui) = ctx.ui() {
                    ui.set_daemon_online(online);
                }
                if !online {
                    ctx.supervise_daemon();
                }
                if let Ok(states) = states {
                    *ctx.states.borrow_mut() = states;
                }
                if let Ok(groups) = groups {
                    *ctx.groups.borrow_mut() = groups;
                }
                if let Ok(presets) = presets {
                    *ctx.presets.borrow_mut() = presets;
                }
                ctx.sync_models();
            },
        );
    }

    /// The daemon is not answering: try to bring it back, at most once per 10 s.
    /// Marks every card offline meanwhile so the UI never lies about reachability.
    fn supervise_daemon(self: &Rc<Self>) {
        if self.last_daemon_attempt.get().elapsed() < Duration::from_secs(10) {
            return;
        }
        self.last_daemon_attempt.set(Instant::now());
        {
            let mut states = self.states.borrow_mut();
            for s in states.iter_mut() {
                s.reachable = false;
            }
        }
        self.sync_models();
        if let Some(ui) = self.ui() {
            ui.set_daemon_status(SharedString::from("Daemon not responding, restarting…"));
        }
        let ctx = Rc::clone(self);
        self.fetcher.run(
            |_api| daemon::ensure_running(),
            move |status| {
                if let Some(ui) = ctx.ui() {
                    ui.set_daemon_status(SharedString::from(status.text()));
                    ui.set_daemon_online(!matches!(status, daemon::DaemonStatus::Unavailable(_)));
                }
                ctx.force_snapshot();
            },
        );
    }

    /// Force a snapshot regardless of the debounce (after management actions).
    fn force_snapshot(self: &Rc<Self>) {
        self.last_snapshot_request
            .set(Instant::now() - Duration::from_secs(5));
        self.request_snapshot();
    }

    /// Push `states`/`groups` into the Slint models, in place where possible.
    fn sync_models(&self) {
        if self.in_grace() {
            return;
        }
        let Some(ui) = self.ui() else { return };
        let states = self.states.borrow();
        let groups = self.groups.borrow();
        let presets = self.presets.borrow();

        // ---- presets (chip labels + settings rows) ----
        let names: Vec<SharedString> = presets
            .iter()
            .map(|p| SharedString::from(p.name.as_str()))
            .collect();
        let current = ui.get_presets_model();
        let same = current.row_count() == names.len()
            && names
                .iter()
                .enumerate()
                .all(|(i, n)| current.row_data(i).as_ref() == Some(n));
        if !same {
            ui.set_presets_model(ModelRc::from(Rc::new(VecModel::from(names))));
            let rows: Vec<PresetRow> = presets
                .iter()
                .map(|p| PresetRow {
                    name: SharedString::from(p.name.as_str()),
                    summary: SharedString::from(p.summary()),
                })
                .collect();
            ui.set_presets_manage_model(ModelRc::from(Rc::new(VecModel::from(rows))));
        } else {
            // Names unchanged; summaries may have changed (re-saved preset).
            let rows = ui.get_presets_manage_model();
            for (i, p) in presets.iter().enumerate() {
                if let Some(mut r) = rows.row_data(i) {
                    let summary = SharedString::from(p.summary());
                    if r.summary != summary {
                        r.summary = summary;
                        rows.set_row_data(i, r);
                    }
                }
            }
        }

        // ---- lights ----
        let all_sticky = match self.all_sliders.get() {
            Some(v) => v,
            None => {
                let refs: Vec<&LightStateResponse> = states.iter().collect();
                let (b, w, _, _) = aggregate(&refs);
                self.all_sliders.set(Some((b, w)));
                (b, w)
            }
        };
        let mut entries = vec![all_card(&states, &presets, all_sticky)];
        for s in states.iter() {
            entries.push(light_card(s, &presets));
        }
        let model = ui.get_lights_model();
        let same_shape = model.row_count() == entries.len()
            && entries
                .iter()
                .enumerate()
                .all(|(i, e)| model.row_data(i).map(|d| d.id == e.id).unwrap_or(false));
        if same_shape {
            for (i, e) in entries.into_iter().enumerate() {
                if model.row_data(i).map(|d| d != e).unwrap_or(true) {
                    model.set_row_data(i, e);
                }
            }
        } else {
            ui.set_lights_model(ModelRc::from(Rc::new(VecModel::from(entries))));
        }

        // ---- groups ----
        let entries: Vec<GroupData> = {
            let mut sticky = self.group_sliders.borrow_mut();
            groups
                .iter()
                .map(|g| {
                    let members: Vec<&LightStateResponse> = states
                        .iter()
                        .filter(|s| g.members.contains(&s.id))
                        .collect();
                    let pos = *sticky.entry(g.name.clone()).or_insert_with(|| {
                        let (b, w, _, _) = aggregate(&members);
                        (b, w)
                    });
                    group_card(g, &members, &presets, pos)
                })
                .collect()
        };
        let model = ui.get_groups_model();
        let same_shape = model.row_count() == entries.len()
            && entries
                .iter()
                .enumerate()
                .all(|(i, e)| model.row_data(i).map(|d| d.name == e.name).unwrap_or(false));
        if same_shape {
            for (i, e) in entries.into_iter().enumerate() {
                if model.row_data(i).map(|d| d != e).unwrap_or(true) {
                    model.set_row_data(i, e);
                }
            }
        } else {
            ui.set_groups_model(ModelRc::from(Rc::new(VecModel::from(entries))));
        }
    }

    /// Reflect a per-light update result: flip reachability on the card.
    fn apply_update_result(
        &self,
        target: &UpdateTarget,
        result: &Result<limelight_core::api::UpdateResponse, api::ApiError>,
    ) {
        let Some(ui) = self.ui() else { return };
        let model = ui.get_lights_model();
        match result {
            Ok(resp) => {
                let mut states = self.states.borrow_mut();
                for r in &resp.results {
                    if let Some(s) = states.iter_mut().find(|s| s.id == r.id) {
                        s.reachable = r.ok;
                        if let Some(new) = &r.state {
                            s.on = new.on;
                            s.brightness = new.brightness;
                            s.kelvin = new.kelvin;
                        }
                    }
                    for i in 1..model.row_count() {
                        if let Some(mut d) = model.row_data(i) {
                            if d.id.as_str() == r.id && d.reachable != r.ok {
                                d.reachable = r.ok;
                                model.set_row_data(i, d);
                            }
                        }
                    }
                }
                ui.set_daemon_online(true);
            }
            Err(err) => {
                eprintln!("update {:?} failed: {err}", target);
                // Daemon-level failure (not per light): mark the target unreachable.
                if let UpdateTarget::Light(id) = target {
                    for i in 1..model.row_count() {
                        if let Some(mut d) = model.row_data(i) {
                            if d.id.as_str() == id {
                                d.reachable = false;
                                model.set_row_data(i, d);
                            }
                        }
                    }
                }
                if err.0.contains("connect") || err.0.contains("refused") {
                    ui.set_daemon_online(false);
                }
            }
        }
    }

    /// Mirror a slider drag into the cached states so member-based highlights
    /// (All Lights, groups) are computed from current values.
    fn set_states_brightness(&self, data: &LightData, brightness: u8) {
        let mut states = self.states.borrow_mut();
        for s in states.iter_mut() {
            if data.is_all || s.id == data.id.as_str() {
                s.brightness = brightness;
            }
        }
    }

    fn set_states_kelvin(&self, data: &LightData, kelvin: u16) {
        let mut states = self.states.borrow_mut();
        for s in states.iter_mut() {
            if data.is_all || s.id == data.id.as_str() {
                s.kelvin = kelvin;
            }
        }
    }

    /// A group control changed: mirror it onto the member cards on the Lights
    /// tab so individual cards always show their real level.
    fn update_light_rows_for_group(&self, group: &str, f: impl Fn(&mut LightData)) {
        let Some(ui) = self.ui() else { return };
        let member_ids: Vec<String> = self
            .groups
            .borrow()
            .iter()
            .find(|g| g.name == group)
            .map(|g| g.members.clone())
            .unwrap_or_default();
        let model = ui.get_lights_model();
        for i in 1..model.row_count() {
            if let Some(mut d) = model.row_data(i) {
                if member_ids.iter().any(|m| m == d.id.as_str()) {
                    f(&mut d);
                    model.set_row_data(i, d);
                }
            }
        }
        refresh_all_card(&model);
    }

    fn set_group_states(&self, group: &str, f: impl Fn(&mut LightStateResponse)) {
        let groups = self.groups.borrow();
        let mut states = self.states.borrow_mut();
        if let Some(g) = groups.iter().find(|g| g.name == group) {
            for s in states.iter_mut() {
                if g.members.contains(&s.id) {
                    f(s);
                }
            }
        }
    }

    fn send(&self, target: UpdateTarget, update: UpdateRequest) {
        self.touched();
        let _ = self.cmd_tx.send(UpdateCommand { target, update });
        // Every user change goes through here, so this is the one place the
        // preset highlights are re-derived from the cards' current values.
        self.recompute_presets();
    }

    /// Re-derive `active_preset` for every light and group card from the
    /// values the card currently shows. Highlight rule: reachable, powered
    /// on, and values within tolerance of the preset. Nothing else.
    fn recompute_presets(&self) {
        let Some(ui) = self.ui() else { return };
        let presets = self.presets.borrow();

        let states = self.states.borrow();
        let lights = ui.get_lights_model();
        for i in 0..lights.row_count() {
            if let Some(mut d) = lights.row_data(i) {
                let active = if d.is_all {
                    let refs: Vec<&LightStateResponse> = states.iter().collect();
                    active_preset_for_members(&presets, &refs)
                } else if d.reachable {
                    active_preset(
                        &presets,
                        d.power_on,
                        slider_to_brightness(d.brightness),
                        warmth_to_kelvin(d.warmth),
                    )
                } else {
                    SharedString::default()
                };
                if d.active_preset != active {
                    d.active_preset = active;
                    lights.set_row_data(i, d);
                }
            }
        }

        let group_defs = self.groups.borrow();
        let groups = ui.get_groups_model();
        for i in 0..groups.row_count() {
            if let Some(mut g) = groups.row_data(i) {
                let members: Vec<&LightStateResponse> = group_defs
                    .iter()
                    .find(|def| def.name == g.name.as_str())
                    .map(|def| {
                        states
                            .iter()
                            .filter(|s| def.members.contains(&s.id))
                            .collect()
                    })
                    .unwrap_or_default();
                let active = active_preset_for_members(&presets, &members);
                if g.active_preset != active {
                    g.active_preset = active;
                    groups.set_row_data(i, g);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Model builders
// ---------------------------------------------------------------------------

/// Name of the preset the given values sit on, if any.
fn active_preset(presets: &[Preset], on: bool, brightness: u8, kelvin: u16) -> SharedString {
    presets
        .iter()
        .find(|p| p.matches(on, brightness, kelvin))
        .map(|p| SharedString::from(p.name.as_str()))
        .unwrap_or_default()
}

fn light_card(s: &LightStateResponse, presets: &[Preset]) -> LightData {
    LightData {
        id: SharedString::from(s.id.as_str()),
        name: SharedString::from(s.display_name()),
        brightness: brightness_to_slider(s.brightness),
        warmth: kelvin_to_warmth(s.kelvin),
        power_on: s.on,
        is_all: false,
        reachable: s.reachable,
        color_capable: s.color_capable,
        active_preset: if s.reachable {
            active_preset(presets, s.on, s.brightness, s.kelvin)
        } else {
            SharedString::default()
        },
    }
}

/// Average of the reachable lights; power = any on.
fn aggregate(states: &[&LightStateResponse]) -> (f32, f32, bool, bool) {
    let reachable: Vec<&&LightStateResponse> = states.iter().filter(|s| s.reachable).collect();
    let any_on = states.iter().any(|s| s.on);
    if reachable.is_empty() {
        return (0.5, 0.5, any_on, false);
    }
    let n = reachable.len() as f32;
    let b = reachable
        .iter()
        .map(|s| brightness_to_slider(s.brightness))
        .sum::<f32>()
        / n;
    let w = reachable
        .iter()
        .map(|s| kelvin_to_warmth(s.kelvin))
        .sum::<f32>()
        / n;
    (b, w, any_on, true)
}

/// Preset that *every* reachable member is on (at least one member required).
fn active_preset_for_members(presets: &[Preset], members: &[&LightStateResponse]) -> SharedString {
    let reachable: Vec<&&LightStateResponse> = members.iter().filter(|m| m.reachable).collect();
    if reachable.is_empty() {
        return SharedString::default();
    }
    presets
        .iter()
        .find(|p| {
            reachable
                .iter()
                .all(|m| p.matches(m.on, m.brightness, m.kelvin))
        })
        .map(|p| SharedString::from(p.name.as_str()))
        .unwrap_or_default()
}

/// All Lights card: sliders are the sticky master position; power = any on;
/// chip = every reachable light on that preset.
fn all_card(states: &[LightStateResponse], presets: &[Preset], sliders: (f32, f32)) -> LightData {
    let refs: Vec<&LightStateResponse> = states.iter().collect();
    let (_, _, on, reachable) = aggregate(&refs);
    LightData {
        id: SharedString::from("__all__"),
        name: SharedString::from("All Lights"),
        brightness: sliders.0,
        warmth: sliders.1,
        power_on: on,
        is_all: true,
        reachable: reachable || states.is_empty(),
        color_capable: false,
        active_preset: active_preset_for_members(presets, &refs),
    }
}

fn group_card(
    g: &Group,
    members: &[&LightStateResponse],
    presets: &[Preset],
    sliders: (f32, f32),
) -> GroupData {
    let (_, _, on, reachable) = aggregate(members);
    GroupData {
        name: SharedString::from(g.name.as_str()),
        brightness: sliders.0,
        warmth: sliders.1,
        power_on: on,
        reachable,
        member_count: members.len() as i32,
        active_preset: active_preset_for_members(presets, members),
    }
}

fn manage_entries(lights: &[LightRecord], states: &[LightStateResponse]) -> Vec<ManageLightEntry> {
    lights
        .iter()
        .map(|rec| ManageLightEntry {
            id: SharedString::from(rec.id.as_str()),
            name: SharedString::from(rec.name.as_str()),
            alias: SharedString::from(rec.alias.as_deref().unwrap_or("")),
            enabled: rec.enabled,
            reachable: states.iter().any(|s| s.id == rec.id && s.reachable),
        })
        .collect()
}

fn set_window_icon(ui: &MainWindow) {
    let icon_bytes = include_bytes!("../assets/Limecon-256.png");
    if let Ok(img) = image::load_from_memory(icon_bytes) {
        let rgba = img.to_rgba8();
        let (w, h) = (rgba.width(), rgba.height());
        if let Ok(icon) = winit::window::Icon::from_rgba(rgba.into_raw(), w, h) {
            ui.window().with_winit_window(|w| {
                w.set_window_icon(Some(icon));
            });
        }
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() -> Result<(), slint::PlatformError> {
    let ui = MainWindow::new()?;
    let xdg_app_id = std::env::var("FLATPAK_ID").unwrap_or_else(|_| autostart::APP_ID.into());
    if let Err(e) = slint::set_xdg_app_id(xdg_app_id.clone()) {
        eprintln!("set_xdg_app_id({xdg_app_id}) failed: {e}");
    }
    set_window_icon(&ui);
    ui.set_app_version(SharedString::from(limelight_core::VERSION));
    ui.set_daemon_status(SharedString::from("Connecting to daemon…"));
    ui.set_daemon_online(false);
    ui.set_autostart_enabled(autostart::enabled());

    // Empty models so the window paints immediately.
    ui.set_lights_model(ModelRc::from(Rc::new(VecModel::from(vec![all_card(
        &[],
        &[],
        (0.5, 0.5),
    )]))));
    ui.set_presets_model(ModelRc::from(Rc::new(VecModel::<SharedString>::default())));
    ui.set_presets_manage_model(ModelRc::from(Rc::new(VecModel::<PresetRow>::default())));
    ui.set_groups_model(ModelRc::from(Rc::new(VecModel::<GroupData>::default())));

    let api = ApiClient::new();
    let fetcher = Fetcher::spawn(api.clone());

    // Update results come back on the UI thread through this handler.
    let ui_weak_for_results = ui.as_weak();
    let (result_tx, result_rx) = mpsc::channel::<(
        UpdateTarget,
        Result<limelight_core::api::UpdateResponse, api::ApiError>,
    )>();
    let handler: update_queue::ResultHandler = Arc::new(move |target, result| {
        let _ = result_tx.send((target, result));
        let _ = ui_weak_for_results.upgrade_in_event_loop(|ui| ui.invoke_update_results_ready());
    });
    let cmd_tx = update_queue::spawn(api, handler);

    let ctx = Rc::new(Ctx {
        ui: ui.as_weak(),
        fetcher,
        cmd_tx,
        states: RefCell::new(Vec::new()),
        groups: RefCell::new(Vec::new()),
        presets: RefCell::new(Vec::new()),
        all_sliders: Cell::new(None),
        group_sliders: RefCell::new(HashMap::new()),
        dragging: Cell::new(false),
        last_user_change: Cell::new(Instant::now() - Duration::from_secs(10)),
        last_snapshot_request: Cell::new(Instant::now() - Duration::from_secs(10)),
        last_daemon_attempt: Cell::new(Instant::now() - Duration::from_secs(60)),
        window_focused: Cell::new(true),
    });

    // Drain update results (runs on the UI thread).
    ui.on_update_results_ready({
        let ctx = Rc::clone(&ctx);
        let rx = Rc::new(result_rx);
        move || {
            while let Ok((target, result)) = rx.try_recv() {
                ctx.apply_update_result(&target, &result);
            }
        }
    });

    // 1) Make sure the daemon is up (on the fetch thread, window already visible).
    // 2) Load settings. 3) First snapshot.
    {
        let ctx = Rc::clone(&ctx);
        ctx.fetcher.run(
            |api| {
                let status = daemon::ensure_running();
                let settings = api.get_settings();
                (status, settings)
            },
            {
                let ctx = Rc::clone(&ctx);
                move |(status, settings): (
                    daemon::DaemonStatus,
                    Result<Settings, api::ApiError>,
                )| {
                    if let Some(ui) = ctx.ui() {
                        ui.set_daemon_status(SharedString::from(status.text()));
                        ui.set_daemon_online(!matches!(
                            status,
                            daemon::DaemonStatus::Unavailable(_)
                        ));
                        if let Ok(s) = settings {
                            ui.set_auto_enable_discovered(s.auto_enable_discovered);
                        }
                    }
                    ctx.force_snapshot();
                }
            },
        );
    }

    // Periodic poll while focused (keeps up with Stream Deck / other controllers).
    let poll_timer = slint::Timer::default();
    poll_timer.start(slint::TimerMode::Repeated, POLL_INTERVAL, {
        let ctx = Rc::clone(&ctx);
        move || {
            if ctx.window_focused.get() {
                ctx.request_snapshot();
            }
        }
    });

    // ---- Nav tabs ----
    ui.on_nav_lights({
        let ctx = Rc::clone(&ctx);
        move || ctx.request_snapshot()
    });
    ui.on_nav_groups({
        let ctx = Rc::clone(&ctx);
        move || ctx.request_snapshot()
    });
    ui.on_nav_settings({
        let ctx = Rc::clone(&ctx);
        move || {
            let ctx2 = Rc::clone(&ctx);
            ctx.fetcher.run(
                |api| (api.health(), api.get_settings()),
                move |(health, settings)| {
                    let Some(ui) = ctx2.ui() else { return };
                    match health {
                        Ok(h) => {
                            ui.set_daemon_status(SharedString::from(format!(
                                "Daemon running (v{}) · {} of {} lights reachable",
                                h.version, h.reachable, h.enabled
                            )));
                            ui.set_daemon_online(true);
                        }
                        Err(e) => {
                            ui.set_daemon_status(SharedString::from(format!(
                                "Daemon unavailable: {e}"
                            )));
                            ui.set_daemon_online(false);
                        }
                    }
                    if let Ok(s) = settings {
                        ui.set_auto_enable_discovered(s.auto_enable_discovered);
                    }
                },
            );
        }
    });

    // ---- Light cards ----
    ui.on_light_brightness_changed({
        let ctx = Rc::clone(&ctx);
        move |idx, value| {
            let Some(ui) = ctx.ui() else { return };
            ctx.dragging.set(true);
            let model = ui.get_lights_model();
            let idx = idx as usize;
            if let Some(mut data) = model.row_data(idx) {
                data.brightness = value;
                model.set_row_data(idx, data.clone());
                if data.is_all {
                    ctx.all_sliders.set(Some((value, data.warmth)));
                    for i in 1..model.row_count() {
                        if let Some(mut d) = model.row_data(i) {
                            d.brightness = value;
                            model.set_row_data(i, d);
                        }
                    }
                } else {
                    refresh_all_card(&model);
                }
                ctx.set_states_brightness(&data, slider_to_brightness(value));
                ctx.send(
                    target_for(&data),
                    UpdateRequest {
                        brightness: Some(slider_to_brightness(value)),
                        ..Default::default()
                    },
                );
            }
        }
    });

    ui.on_light_warmth_changed({
        let ctx = Rc::clone(&ctx);
        move |idx, value| {
            let Some(ui) = ctx.ui() else { return };
            ctx.dragging.set(true);
            let model = ui.get_lights_model();
            let idx = idx as usize;
            if let Some(mut data) = model.row_data(idx) {
                data.warmth = value;
                model.set_row_data(idx, data.clone());
                if data.is_all {
                    ctx.all_sliders.set(Some((data.brightness, value)));
                    for i in 1..model.row_count() {
                        if let Some(mut d) = model.row_data(i) {
                            d.warmth = value;
                            model.set_row_data(i, d);
                        }
                    }
                } else {
                    refresh_all_card(&model);
                }
                ctx.set_states_kelvin(&data, warmth_to_kelvin(value));
                ctx.send(
                    target_for(&data),
                    UpdateRequest {
                        kelvin: Some(warmth_to_kelvin(value)),
                        ..Default::default()
                    },
                );
            }
        }
    });

    ui.on_light_power_toggled({
        let ctx = Rc::clone(&ctx);
        move |idx| {
            let Some(ui) = ctx.ui() else { return };
            let model = ui.get_lights_model();
            let idx = idx as usize;
            if let Some(mut data) = model.row_data(idx) {
                let new_power = !data.power_on;
                data.power_on = new_power;
                model.set_row_data(idx, data.clone());
                if data.is_all {
                    for i in 1..model.row_count() {
                        if let Some(mut d) = model.row_data(i) {
                            d.power_on = new_power;
                            model.set_row_data(i, d);
                        }
                    }
                } else {
                    refresh_all_card(&model);
                }
                {
                    let mut states = ctx.states.borrow_mut();
                    for s in states.iter_mut() {
                        if data.is_all || s.id == data.id.as_str() {
                            s.on = new_power;
                        }
                    }
                }
                ctx.send(
                    target_for(&data),
                    UpdateRequest {
                        on: Some(u8::from(new_power)),
                        ..Default::default()
                    },
                );
            }
        }
    });

    ui.on_light_slider_released({
        let ctx = Rc::clone(&ctx);
        move |idx| {
            ctx.dragging.set(false);
            ctx.touched();
            let Some(ui) = ctx.ui() else { return };
            let model = ui.get_lights_model();
            if let Some(data) = model.row_data(idx as usize) {
                // Final authoritative values for this target.
                ctx.send(
                    target_for(&data),
                    UpdateRequest {
                        brightness: Some(slider_to_brightness(data.brightness)),
                        kelvin: Some(warmth_to_kelvin(data.warmth)),
                        ..Default::default()
                    },
                );
                let mut states = ctx.states.borrow_mut();
                for s in states.iter_mut() {
                    if data.is_all || s.id == data.id.as_str() {
                        s.brightness = slider_to_brightness(data.brightness);
                        s.kelvin = warmth_to_kelvin(data.warmth);
                    }
                }
            }
        }
    });

    // ---- Group cards ----
    ui.on_group_brightness_changed({
        let ctx = Rc::clone(&ctx);
        move |idx, value| {
            let Some(ui) = ctx.ui() else { return };
            ctx.dragging.set(true);
            let model = ui.get_groups_model();
            let idx = idx as usize;
            if let Some(mut data) = model.row_data(idx) {
                data.brightness = value;
                model.set_row_data(idx, data.clone());
                ctx.group_sliders
                    .borrow_mut()
                    .insert(data.name.to_string(), (value, data.warmth));
                ctx.set_group_states(&data.name, |s| s.brightness = slider_to_brightness(value));
                ctx.update_light_rows_for_group(&data.name, |d| d.brightness = value);
                ctx.send(
                    UpdateTarget::Group(data.name.to_string()),
                    UpdateRequest {
                        brightness: Some(slider_to_brightness(value)),
                        ..Default::default()
                    },
                );
            }
        }
    });

    ui.on_group_warmth_changed({
        let ctx = Rc::clone(&ctx);
        move |idx, value| {
            let Some(ui) = ctx.ui() else { return };
            ctx.dragging.set(true);
            let model = ui.get_groups_model();
            let idx = idx as usize;
            if let Some(mut data) = model.row_data(idx) {
                data.warmth = value;
                model.set_row_data(idx, data.clone());
                ctx.group_sliders
                    .borrow_mut()
                    .insert(data.name.to_string(), (data.brightness, value));
                ctx.set_group_states(&data.name, |s| s.kelvin = warmth_to_kelvin(value));
                ctx.update_light_rows_for_group(&data.name, |d| d.warmth = value);
                ctx.send(
                    UpdateTarget::Group(data.name.to_string()),
                    UpdateRequest {
                        kelvin: Some(warmth_to_kelvin(value)),
                        ..Default::default()
                    },
                );
            }
        }
    });

    ui.on_group_power_toggled({
        let ctx = Rc::clone(&ctx);
        move |idx| {
            let Some(ui) = ctx.ui() else { return };
            let model = ui.get_groups_model();
            let idx = idx as usize;
            if let Some(mut data) = model.row_data(idx) {
                let new_power = !data.power_on;
                data.power_on = new_power;
                model.set_row_data(idx, data.clone());
                let name = data.name.to_string();
                {
                    let groups = ctx.groups.borrow();
                    let mut states = ctx.states.borrow_mut();
                    if let Some(g) = groups.iter().find(|g| g.name == name) {
                        for s in states.iter_mut() {
                            if g.members.contains(&s.id) {
                                s.on = new_power;
                            }
                        }
                    }
                }
                ctx.update_light_rows_for_group(&name, |d| d.power_on = new_power);
                ctx.send(
                    UpdateTarget::Group(name),
                    UpdateRequest {
                        on: Some(u8::from(new_power)),
                        ..Default::default()
                    },
                );
            }
        }
    });

    ui.on_group_slider_released({
        let ctx = Rc::clone(&ctx);
        move |idx| {
            ctx.dragging.set(false);
            ctx.touched();
            let Some(ui) = ctx.ui() else { return };
            let model = ui.get_groups_model();
            if let Some(data) = model.row_data(idx as usize) {
                let name = data.name.to_string();
                let (b, k) = (
                    slider_to_brightness(data.brightness),
                    warmth_to_kelvin(data.warmth),
                );
                {
                    let groups = ctx.groups.borrow();
                    let mut states = ctx.states.borrow_mut();
                    if let Some(g) = groups.iter().find(|g| g.name == name) {
                        for s in states.iter_mut() {
                            if g.members.contains(&s.id) {
                                s.brightness = b;
                                s.kelvin = k;
                            }
                        }
                    }
                }
                ctx.send(
                    UpdateTarget::Group(name),
                    UpdateRequest {
                        brightness: Some(b),
                        kelvin: Some(k),
                        ..Default::default()
                    },
                );
            }
        }
    });

    // ---- Window management ----
    ui.on_request_quit(|| {
        let _ = slint::quit_event_loop();
    });

    ui.on_request_minimize({
        let ui_weak = ui.as_weak();
        move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.window().with_winit_window(|w| w.set_minimized(true));
            }
        }
    });

    // ---- Manage lights panel ----
    let load_manage = {
        let ctx = Rc::clone(&ctx);
        move |scan: bool| {
            let ctx2 = Rc::clone(&ctx);
            if scan {
                if let Some(ui) = ctx.ui() {
                    ui.set_scanning(true);
                }
            }
            ctx.fetcher.run(
                move |api| {
                    if scan {
                        if let Err(e) = api.refresh() {
                            eprintln!("scan failed: {e}");
                        }
                    }
                    let lights = api.get_lights().unwrap_or_default();
                    let states = api.get_states().unwrap_or_default();
                    (lights, states)
                },
                move |(lights, states)| {
                    let Some(ui) = ctx2.ui() else { return };
                    ui.set_manage_model(ModelRc::from(Rc::new(VecModel::from(manage_entries(
                        &lights, &states,
                    )))));
                    ui.set_scanning(false);
                    *ctx2.states.borrow_mut() = states;
                    ctx2.sync_models();
                },
            );
        }
    };

    ui.on_request_add({
        let load_manage = load_manage.clone();
        move || load_manage(false)
    });
    ui.on_request_scan({
        let load_manage = load_manage.clone();
        move || load_manage(true)
    });

    ui.on_manage_enable_toggled({
        let ctx = Rc::clone(&ctx);
        move |idx| {
            let Some(ui) = ctx.ui() else { return };
            let model = ui.get_manage_model();
            let idx = idx as usize;
            if let Some(mut entry) = model.row_data(idx) {
                let enabled = !entry.enabled;
                entry.enabled = enabled;
                model.set_row_data(idx, entry.clone());
                let id = entry.id.to_string();
                let ctx2 = Rc::clone(&ctx);
                ctx.fetcher.run(
                    move |api| api.set_enabled(&id, enabled).map(|_| ()),
                    move |res| {
                        if let Err(e) = res {
                            eprintln!("set_enabled failed: {e}");
                        }
                        ctx2.force_snapshot();
                    },
                );
            }
        }
    });

    ui.on_manage_rename({
        let ctx = Rc::clone(&ctx);
        move |idx, new_name| {
            let Some(ui) = ctx.ui() else { return };
            let model = ui.get_manage_model();
            let idx = idx as usize;
            if let Some(mut entry) = model.row_data(idx) {
                let alias = new_name.to_string();
                entry.alias = SharedString::from(alias.trim());
                model.set_row_data(idx, entry.clone());
                let id = entry.id.to_string();
                let ctx2 = Rc::clone(&ctx);
                ctx.fetcher.run(
                    move |api| api.set_alias(&id, &alias).map(|_| ()),
                    move |res| {
                        if let Err(e) = res {
                            eprintln!("set_alias failed: {e}");
                        }
                        ctx2.force_snapshot();
                    },
                );
            }
        }
    });

    ui.on_manage_delete({
        let ctx = Rc::clone(&ctx);
        move |idx| {
            let Some(ui) = ctx.ui() else { return };
            let manage = ui.get_manage_model();
            let idx = idx as usize;
            if let Some(entry) = manage.row_data(idx) {
                let entries: Vec<ManageLightEntry> = (0..manage.row_count())
                    .filter(|i| *i != idx)
                    .filter_map(|i| manage.row_data(i))
                    .collect();
                ui.set_manage_model(ModelRc::from(Rc::new(VecModel::from(entries))));
                let id = entry.id.to_string();
                ctx.states.borrow_mut().retain(|s| s.id != id);
                ctx.sync_models();
                let ctx2 = Rc::clone(&ctx);
                ctx.fetcher.run(
                    move |api| api.delete_light(&id),
                    move |res| {
                        if let Err(e) = res {
                            eprintln!("delete_light failed: {e}");
                        }
                        ctx2.force_snapshot();
                    },
                );
            }
        }
    });

    ui.on_manage_close({
        let ctx = Rc::clone(&ctx);
        move || ctx.force_snapshot()
    });

    // ---- Manage groups panel ----
    ui.on_request_groups_panel({
        let ctx = Rc::clone(&ctx);
        move || {
            let ctx2 = Rc::clone(&ctx);
            ctx.fetcher.run(
                |api| api.get_states().unwrap_or_default(),
                move |states| {
                    let Some(ui) = ctx2.ui() else { return };
                    let picks: Vec<GroupLightPick> = states
                        .iter()
                        .filter(|s| s.enabled)
                        .map(|s| GroupLightPick {
                            id: SharedString::from(s.id.as_str()),
                            name: SharedString::from(s.display_name()),
                            selected: false,
                        })
                        .collect();
                    ui.set_group_lights_pick(ModelRc::from(Rc::new(VecModel::from(picks))));
                },
            );
        }
    });

    ui.on_group_pick_toggled({
        let ui_weak = ui.as_weak();
        move |idx| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let model = ui.get_group_lights_pick();
            let idx = idx as usize;
            if let Some(mut entry) = model.row_data(idx) {
                entry.selected = !entry.selected;
                model.set_row_data(idx, entry);
            }
        }
    });

    ui.on_group_manage_save({
        let ctx = Rc::clone(&ctx);
        move |name| {
            let name = name.trim().to_string();
            if name.is_empty() {
                return;
            }
            let Some(ui) = ctx.ui() else { return };
            let picks = ui.get_group_lights_pick();
            let members: Vec<String> = (0..picks.row_count())
                .filter_map(|i| picks.row_data(i))
                .filter(|p| p.selected)
                .map(|p| p.id.to_string())
                .collect();
            if members.is_empty() {
                return;
            }
            let ctx2 = Rc::clone(&ctx);
            ctx.fetcher.run(
                move |api| api.create_group(&name, &members).map(|_| ()),
                move |res| {
                    match res {
                        Ok(()) => {
                            if let Some(ui) = ctx2.ui() {
                                ui.set_groups_panel_open(false);
                            }
                        }
                        Err(e) => eprintln!("create_group failed: {e}"),
                    }
                    ctx2.force_snapshot();
                },
            );
        }
    });

    ui.on_group_manage_delete({
        let ctx = Rc::clone(&ctx);
        move |idx| {
            let Some(ui) = ctx.ui() else { return };
            let model = ui.get_groups_model();
            let idx = idx as usize;
            if let Some(data) = model.row_data(idx) {
                let name = data.name.to_string();
                ctx.groups.borrow_mut().retain(|g| g.name != name);
                ctx.sync_models();
                let ctx2 = Rc::clone(&ctx);
                ctx.fetcher.run(
                    move |api| api.delete_group(&name),
                    move |res| {
                        if let Err(e) = res {
                            eprintln!("delete_group failed: {e}");
                        }
                        ctx2.force_snapshot();
                    },
                );
            }
        }
    });

    ui.on_group_manage_close({
        let ctx = Rc::clone(&ctx);
        move || ctx.force_snapshot()
    });

    // ---- Settings ----
    ui.on_autostart_toggled({
        let ui_weak = ui.as_weak();
        move |enabled| {
            let result = if enabled {
                autostart::enable()
            } else {
                autostart::disable()
            };
            if let Err(e) = result {
                eprintln!("autostart change failed: {e}");
            }
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_autostart_enabled(autostart::enabled());
            }
        }
    });

    ui.on_auto_enable_toggled({
        let ctx = Rc::clone(&ctx);
        move |enabled| {
            if let Some(ui) = ctx.ui() {
                ui.set_auto_enable_discovered(enabled);
            }
            let ctx2 = Rc::clone(&ctx);
            ctx.fetcher.run(
                move |api| {
                    let mut s = api.get_settings().unwrap_or_default();
                    s.auto_enable_discovered = enabled;
                    api.set_settings(&s)
                },
                move |res| match res {
                    Ok(s) => {
                        if let Some(ui) = ctx2.ui() {
                            ui.set_auto_enable_discovered(s.auto_enable_discovered);
                        }
                    }
                    Err(e) => eprintln!("set_settings failed: {e}"),
                },
            );
        }
    });

    // ---- Presets ----
    ui.on_light_preset_tapped({
        let ctx = Rc::clone(&ctx);
        move |idx, name| {
            let Some(ui) = ctx.ui() else { return };
            let Some(preset) = ctx
                .presets
                .borrow()
                .iter()
                .find(|p| p.name == name.as_str())
                .cloned()
            else {
                return;
            };
            let model = ui.get_lights_model();
            let idx = idx as usize;
            let Some(mut data) = model.row_data(idx) else {
                return;
            };
            let (b, w) = (
                brightness_to_slider(preset.brightness),
                kelvin_to_warmth(preset.kelvin),
            );
            data.brightness = b;
            data.warmth = w;
            data.power_on = preset.on;
            data.active_preset = name.clone();
            model.set_row_data(idx, data.clone());
            if data.is_all {
                ctx.all_sliders.set(Some((b, w)));
                for i in 1..model.row_count() {
                    if let Some(mut d) = model.row_data(i) {
                        d.brightness = b;
                        d.warmth = w;
                        d.power_on = preset.on;
                        d.active_preset = name.clone();
                        model.set_row_data(i, d);
                    }
                }
            } else {
                refresh_all_card(&model);
            }
            {
                let mut states = ctx.states.borrow_mut();
                for s in states.iter_mut() {
                    if data.is_all || s.id == data.id.as_str() {
                        s.on = preset.on;
                        s.brightness = preset.brightness;
                        s.kelvin = preset.kelvin;
                    }
                }
            }
            ctx.send(target_for(&data), UpdateRequest::from(&preset));
        }
    });

    ui.on_group_preset_tapped({
        let ctx = Rc::clone(&ctx);
        move |idx, name| {
            let Some(ui) = ctx.ui() else { return };
            let Some(preset) = ctx
                .presets
                .borrow()
                .iter()
                .find(|p| p.name == name.as_str())
                .cloned()
            else {
                return;
            };
            let model = ui.get_groups_model();
            let idx = idx as usize;
            let Some(mut data) = model.row_data(idx) else {
                return;
            };
            data.brightness = brightness_to_slider(preset.brightness);
            data.warmth = kelvin_to_warmth(preset.kelvin);
            data.power_on = preset.on;
            data.active_preset = name.clone();
            model.set_row_data(idx, data.clone());
            let group_name = data.name.to_string();
            ctx.group_sliders
                .borrow_mut()
                .insert(group_name.clone(), (data.brightness, data.warmth));
            {
                let (b, w, on) = (data.brightness, data.warmth, preset.on);
                let name_for_rows = name.clone();
                ctx.update_light_rows_for_group(&group_name, |d| {
                    d.brightness = b;
                    d.warmth = w;
                    d.power_on = on;
                    d.active_preset = name_for_rows.clone();
                });
            }
            {
                let groups = ctx.groups.borrow();
                let mut states = ctx.states.borrow_mut();
                if let Some(g) = groups.iter().find(|g| g.name == group_name) {
                    for s in states.iter_mut() {
                        if g.members.contains(&s.id) {
                            s.on = preset.on;
                            s.brightness = preset.brightness;
                            s.kelvin = preset.kelvin;
                        }
                    }
                }
            }
            ctx.send(
                UpdateTarget::Group(group_name),
                UpdateRequest::from(&preset),
            );
        }
    });

    // "+" chip: open the editor prefilled with the card's current values.
    let open_editor_from = {
        let ui_weak = ui.as_weak();
        move |source: String, brightness: f32, warmth: f32| {
            let Some(ui) = ui_weak.upgrade() else { return };
            ui.set_preset_editor(PresetEdit {
                original_name: SharedString::default(),
                name: SharedString::default(),
                brightness,
                warmth,
                is_new: true,
                source: SharedString::from(source),
            });
            ui.set_preset_editor_open(true);
        }
    };

    ui.on_light_save_preset({
        let ui_weak = ui.as_weak();
        let open_editor_from = open_editor_from.clone();
        move |idx| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let Some(data) = ui.get_lights_model().row_data(idx as usize) else {
                return;
            };
            open_editor_from(data.name.to_string(), data.brightness, data.warmth);
        }
    });

    ui.on_group_save_preset({
        let ui_weak = ui.as_weak();
        let open_editor_from = open_editor_from.clone();
        move |idx| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let Some(data) = ui.get_groups_model().row_data(idx as usize) else {
                return;
            };
            open_editor_from(data.name.to_string(), data.brightness, data.warmth);
        }
    });

    // Settings: rename / reorder / delete presets (whole-list replace keeps order).
    let push_presets = {
        let ctx = Rc::clone(&ctx);
        move |presets: Vec<Preset>| {
            *ctx.presets.borrow_mut() = presets.clone();
            ctx.sync_models();
            let ctx2 = Rc::clone(&ctx);
            ctx.fetcher.run(
                move |api| api.set_presets(&presets).map(|_| ()),
                move |res| {
                    if let Err(e) = res {
                        eprintln!("set_presets failed: {e}");
                    }
                    ctx2.force_snapshot();
                },
            );
        }
    };

    ui.on_preset_rename({
        let ctx = Rc::clone(&ctx);
        let push_presets = push_presets.clone();
        move |idx, new_name| {
            let new_name = new_name.trim().to_string();
            let mut presets = ctx.presets.borrow().clone();
            let idx = idx as usize;
            if new_name.is_empty() || idx >= presets.len() || presets[idx].name == new_name {
                return;
            }
            presets[idx].name = new_name;
            push_presets(presets);
        }
    });

    ui.on_preset_move({
        let ctx = Rc::clone(&ctx);
        let push_presets = push_presets.clone();
        move |idx, dir| {
            let mut presets = ctx.presets.borrow().clone();
            let idx = idx as usize;
            let Some(to) = idx.checked_add_signed(dir as isize) else {
                return;
            };
            if idx >= presets.len() || to >= presets.len() {
                return;
            }
            presets.swap(idx, to);
            push_presets(presets);
        }
    });

    ui.on_preset_delete({
        let ctx = Rc::clone(&ctx);
        move |idx| {
            let name = {
                let presets = ctx.presets.borrow();
                let Some(p) = presets.get(idx as usize) else {
                    return;
                };
                p.name.clone()
            };
            ctx.presets.borrow_mut().retain(|p| p.name != name);
            ctx.sync_models();
            let ctx2 = Rc::clone(&ctx);
            ctx.fetcher.run(
                move |api| api.delete_preset(&name),
                move |res| {
                    if let Err(e) = res {
                        eprintln!("delete_preset failed: {e}");
                    }
                    ctx2.force_snapshot();
                },
            );
        }
    });

    // Settings: add / edit presets through the editor panel.
    ui.on_preset_editor_add({
        let ui_weak = ui.as_weak();
        move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            ui.set_preset_editor(PresetEdit {
                original_name: SharedString::default(),
                name: SharedString::default(),
                brightness: 0.5,
                warmth: kelvin_to_warmth(4500),
                is_new: true,
                source: SharedString::default(),
            });
            ui.set_preset_editor_open(true);
        }
    });

    ui.on_preset_editor_edit({
        let ctx = Rc::clone(&ctx);
        move |idx| {
            let Some(ui) = ctx.ui() else { return };
            let Some(p) = ctx.presets.borrow().get(idx as usize).cloned() else {
                return;
            };
            ui.set_preset_editor(PresetEdit {
                original_name: SharedString::from(p.name.as_str()),
                name: SharedString::from(p.name.as_str()),
                brightness: brightness_to_slider(p.brightness),
                warmth: kelvin_to_warmth(p.kelvin),
                is_new: false,
                source: SharedString::default(),
            });
            ui.set_preset_editor_open(true);
        }
    });

    ui.on_preset_editor_save({
        let ctx = Rc::clone(&ctx);
        let push_presets = push_presets.clone();
        move || {
            let Some(ui) = ctx.ui() else { return };
            let edit = ui.get_preset_editor();
            let name = edit.name.trim().to_string();
            if name.is_empty() {
                return;
            }
            let values = Preset {
                name: name.clone(),
                on: true,
                brightness: slider_to_brightness(edit.brightness),
                kelvin: warmth_to_kelvin(edit.warmth),
                hue: None,
                saturation: None,
            };
            ui.set_preset_editor_open(false);
            let original = edit.original_name.to_string();
            if !edit.is_new && !original.is_empty() {
                // Edit in place (handles rename) by replacing the ordered list.
                let mut presets = ctx.presets.borrow().clone();
                match presets.iter_mut().find(|p| p.name == original) {
                    Some(p) => *p = values,
                    None => presets.push(values),
                }
                push_presets(presets);
            } else {
                let ctx2 = Rc::clone(&ctx);
                ctx.fetcher.run(
                    move |api| api.save_preset(&values).map(|_| ()),
                    move |res| {
                        if let Err(e) = res {
                            eprintln!("save_preset failed: {e}");
                        }
                        ctx2.force_snapshot();
                    },
                );
            }
        }
    });

    // ---- Per-light settings panel ----
    ui.on_light_settings_requested({
        let ctx = Rc::clone(&ctx);
        move |idx| {
            let Some(ui) = ctx.ui() else { return };
            let Some(card) = ui.get_lights_model().row_data(idx as usize) else {
                return;
            };
            if card.is_all {
                return;
            }
            let id = card.id.to_string();
            ui.set_light_settings(LightSettingsData {
                id: SharedString::from(id.as_str()),
                status: SharedString::from("Loading…"),
                loaded: false,
                ..Default::default()
            });
            ui.set_light_settings_open(true);
            let ctx2 = Rc::clone(&ctx);
            let id2 = id.clone();
            ctx.fetcher.run(
                move |api| (api.get_record(&id2), api.get_device_settings(&id2)),
                move |(record, settings)| {
                    let Some(ui) = ctx2.ui() else { return };
                    let mut data = ui.get_light_settings();
                    if data.id.as_str() != id {
                        return; // user opened a different light meanwhile
                    }
                    match (record, settings) {
                        (Ok(rec), Ok(s)) => {
                            let info = rec
                                .accessory_info
                                .as_ref()
                                .and_then(limelight_core::elgato::AccessoryInfo::from_value)
                                .unwrap_or_default();
                            data.name = SharedString::from(rec.name.as_str());
                            data.original_name = SharedString::from(rec.name.as_str());
                            data.product = SharedString::from(
                                rec.product.as_deref().unwrap_or("Elgato light"),
                            );
                            data.firmware = SharedString::from(info.firmware_version.as_str());
                            data.serial = SharedString::from(rec.serial.as_deref().unwrap_or(""));
                            data.address = SharedString::from(rec.primary_address().unwrap_or(""));
                            data.restore_last = s.power_on_behavior.unwrap_or(1) != 2;
                            data.default_brightness =
                                brightness_to_slider(s.power_on_brightness.unwrap_or(50));
                            data.default_warmth =
                                kelvin_to_warmth(limelight_core::convert::mired_to_kelvin(
                                    s.power_on_temperature.unwrap_or(213),
                                ));
                            data.switch_on_ms = s.switch_on_duration_ms.unwrap_or(100) as i32;
                            data.switch_off_ms = s.switch_off_duration_ms.unwrap_or(300) as i32;
                            data.color_change_ms = s.color_change_duration_ms.unwrap_or(100) as i32;
                            data.status = SharedString::from("");
                            data.loaded = true;
                        }
                        (Err(e), _) | (_, Err(e)) => {
                            data.status =
                                SharedString::from(format!("Could not load settings: {e}"));
                            data.loaded = false;
                        }
                    }
                    ui.set_light_settings(data);
                },
            );
        }
    });

    ui.on_light_settings_save({
        let ctx = Rc::clone(&ctx);
        move || {
            let Some(ui) = ctx.ui() else { return };
            let mut data = ui.get_light_settings();
            if !data.loaded {
                return;
            }
            data.status = SharedString::from("Saving…");
            ui.set_light_settings(data.clone());
            let id = data.id.to_string();
            let new_name = data.name.trim().to_string();
            let rename = if !new_name.is_empty() && new_name != data.original_name.as_str() {
                Some(new_name)
            } else {
                None
            };
            let settings = limelight_core::elgato::DeviceSettings {
                power_on_behavior: Some(if data.restore_last { 1 } else { 2 }),
                power_on_brightness: Some(slider_to_brightness(data.default_brightness)),
                power_on_temperature: Some(limelight_core::convert::kelvin_to_mired(
                    warmth_to_kelvin(data.default_warmth),
                )),
                switch_on_duration_ms: Some(data.switch_on_ms.max(0) as u32),
                switch_off_duration_ms: Some(data.switch_off_ms.max(0) as u32),
                color_change_duration_ms: Some(data.color_change_ms.max(0) as u32),
                ..Default::default()
            };
            let ctx2 = Rc::clone(&ctx);
            ctx.fetcher.run(
                move |api| {
                    let settings = api.set_device_settings(&id, &settings);
                    let renamed = match &rename {
                        Some(name) => api.rename_device(&id, name).map(|_| true),
                        None => Ok(false),
                    };
                    (settings, renamed)
                },
                move |(settings, renamed)| {
                    let Some(ui) = ctx2.ui() else { return };
                    let mut data = ui.get_light_settings();
                    data.status = SharedString::from(match (&settings, &renamed) {
                        (Ok(_), Ok(_)) => "Saved".to_string(),
                        (Err(e), _) => format!("Settings not saved: {e}"),
                        (_, Err(e)) => format!("Saved, but rename failed: {e}"),
                    });
                    if renamed.as_ref().map(|r| *r).unwrap_or(false) {
                        data.original_name = data.name.clone();
                    }
                    ui.set_light_settings(data);
                    ctx2.force_snapshot();
                },
            );
        }
    });

    ui.on_light_settings_identify({
        let ctx = Rc::clone(&ctx);
        move || {
            let Some(ui) = ctx.ui() else { return };
            let id = ui.get_light_settings().id.to_string();
            if id.is_empty() {
                return;
            }
            ctx.fetcher.fire(move |api| {
                if let Err(e) = api.identify(&id) {
                    eprintln!("identify failed: {e}");
                }
            });
        }
    });

    ui.on_light_settings_close({
        let ctx = Rc::clone(&ctx);
        move || ctx.force_snapshot()
    });

    // Dev aid: LIMELIGHT_OPEN_SETTINGS=1 opens the first light's settings panel
    // shortly after start (used to screenshot / verify the panel without input).
    let dev_open_timer = slint::Timer::default();
    if std::env::var("LIMELIGHT_OPEN_SETTINGS").is_ok() {
        let ui_weak = ui.as_weak();
        dev_open_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(2500),
            move || {
                if let Some(ui) = ui_weak.upgrade() {
                    if ui.get_lights_model().row_count() > 1 {
                        ui.invoke_light_settings_requested(1);
                    }
                }
            },
        );
    }

    // Dev aid: LIMELIGHT_WINDOW_HEIGHT=<px> (screenshots of long tabs).
    if let Some(h) = std::env::var("LIMELIGHT_WINDOW_HEIGHT")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
    {
        ui.window().set_size(slint::LogicalSize::new(400.0, h));
    }
    // Dev aid: LIMELIGHT_OPEN_SAVE_PRESET=1 opens the save-preset panel for the first light.
    let dev_save_timer = slint::Timer::default();
    if std::env::var("LIMELIGHT_OPEN_SAVE_PRESET").is_ok() {
        let ui_weak = ui.as_weak();
        dev_save_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(2500),
            move || {
                if let Some(ui) = ui_weak.upgrade() {
                    if ui.get_lights_model().row_count() > 1 {
                        ui.invoke_light_save_preset(1);
                    }
                }
            },
        );
    }

    // Dev aid: LIMELIGHT_OPEN_PRESET_EDITOR=1 opens the Settings preset editor (add mode).
    let dev_editor_timer = slint::Timer::default();
    if std::env::var("LIMELIGHT_OPEN_PRESET_EDITOR").is_ok() {
        let ui_weak = ui.as_weak();
        dev_editor_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(2500),
            move || {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.invoke_preset_editor_add();
                }
            },
        );
    }

    // Dev aid: LIMELIGHT_OPEN_TAB=0|1|2 selects a tab at start (screenshots).
    if let Some(tab) = std::env::var("LIMELIGHT_OPEN_TAB")
        .ok()
        .and_then(|v| v.parse::<i32>().ok())
    {
        ui.set_selected_tab(tab);
        match tab {
            1 => ui.invoke_nav_groups(),
            2 => ui.invoke_nav_settings(),
            _ => ui.invoke_nav_lights(),
        }
    }

    // ---- Window drag (frameless window) + focus tracking ----
    let is_wayland = std::env::var("WAYLAND_DISPLAY").is_ok();
    let drag_in_progress = Rc::new(RefCell::new(false));
    let cursor_left_during_drag = Rc::new(RefCell::new(false));

    ui.window().on_winit_window_event({
        let drag_in_progress = Rc::clone(&drag_in_progress);
        let cursor_left_during_drag = Rc::clone(&cursor_left_during_drag);
        let ctx = Rc::clone(&ctx);
        move |_w, event| {
            match event {
                WindowEvent::Focused(focused) => {
                    ctx.window_focused.set(*focused);
                    if *focused {
                        ctx.request_snapshot();
                    }
                }
                WindowEvent::CursorMoved { .. } if *drag_in_progress.borrow() => {
                    *drag_in_progress.borrow_mut() = false;
                    *cursor_left_during_drag.borrow_mut() = false;
                    if let Some(ui) = ctx.ui() {
                        ui.invoke_reset_drag_state();
                    }
                }
                WindowEvent::CursorEntered { .. }
                    if *drag_in_progress.borrow() && *cursor_left_during_drag.borrow() =>
                {
                    *drag_in_progress.borrow_mut() = false;
                    *cursor_left_during_drag.borrow_mut() = false;
                    if let Some(ui) = ctx.ui() {
                        ui.invoke_reset_drag_state();
                    }
                }
                WindowEvent::CursorLeft { .. } if *drag_in_progress.borrow() => {
                    *cursor_left_during_drag.borrow_mut() = true;
                }
                _ => {}
            }
            EventResult::Propagate
        }
    });

    ui.on_start_window_drag({
        let drag_in_progress = Rc::clone(&drag_in_progress);
        let ui_weak = ui.as_weak();
        move || {
            if !is_wayland {
                return;
            }
            *drag_in_progress.borrow_mut() = true;
            if let Some(ui) = ui_weak.upgrade() {
                ui.window().with_winit_window(|w| {
                    let _ = w.drag_window();
                    w.set_cursor(winit::window::CursorIcon::Default);
                });
            }
        }
    });

    ui.on_drag_window_by({
        let ui_weak = ui.as_weak();
        move |dx, dy| {
            if is_wayland || (dx == 0 && dy == 0) {
                return;
            }
            if let Some(ui) = ui_weak.upgrade() {
                ui.window().with_winit_window(|w| {
                    if let Ok(pos) = w.outer_position() {
                        w.set_outer_position(winit::dpi::PhysicalPosition::new(
                            pos.x + dx,
                            pos.y + dy,
                        ));
                    }
                });
            }
        }
    });

    ui.run()
}

fn target_for(data: &LightData) -> UpdateTarget {
    if data.is_all {
        UpdateTarget::All
    } else {
        UpdateTarget::Light(data.id.to_string())
    }
}

/// Keep the "All Lights" power button honest (any light on). Its sliders are
/// a master control and are deliberately left where the user put them.
fn refresh_all_card(model: &ModelRc<LightData>) {
    let any_on =
        (1..model.row_count()).any(|i| model.row_data(i).map(|d| d.power_on).unwrap_or(false));
    if let Some(mut all) = model.row_data(0) {
        if all.is_all && all.power_on != any_on {
            all.power_on = any_on;
            model.set_row_data(0, all);
        }
    }
}
