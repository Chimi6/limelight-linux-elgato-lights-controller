//! keylight-gui — LimeLight desktop GUI (Slint).
//! Talks to keylightd HTTP API; see docs/API.md.

mod api;
mod update_queue;

use api::{
    api_to_brightness, brightness_to_api, kelvin_to_warmth, warmth_to_kelvin, ApiClient,
    LightRecord, UpdatePayload,
};
use i_slint_backend_winit::{EventResult, WinitWindowAccessor};
use slint::{Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use api::GroupRecord;
use update_queue::{UpdateCommand, UpdateTarget};
use winit::event::WindowEvent;

slint::include_modules!();

/// Load and set the window icon from the assets (taskbar/dock).
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

fn main() -> Result<(), slint::PlatformError> {
    ensure_daemon_running();

    let api = ApiClient::new();
    let fetch_api = api.clone();

    // Fetch initial lights + states from keylightd (best-effort, blocking)
    let lights = api.get_lights().unwrap_or_default();
    let states = api.get_light_states().unwrap_or_default();
    let groups = api.get_groups().unwrap_or_default();

    // Spawn blocking update queue (moves api into background thread)
    let cmd_tx = update_queue::spawn(api);

    let ui = MainWindow::new()?;
    // Match KDE/Wayland taskbar grouping to the Flatpak desktop id.
    // Slint platform is initialized after MainWindow::new(), but before ui.run().
    let xdg_app_id = std::env::var("FLATPAK_ID")
        .unwrap_or_else(|_| "io.github.chimi6.limelight-linux-elgato-lights-controller".into());
    if let Err(e) = slint::set_xdg_app_id(xdg_app_id.clone()) {
        eprintln!("set_xdg_app_id({xdg_app_id}) failed: {e}");
    }
    set_window_icon(&ui);

    // Build lights model: ALL card first, then individual reachable lights
    let lights_model = Rc::new(VecModel::<LightData>::default());
    {
        let any_on = states.iter().any(|s| s.on);
        lights_model.push(LightData {
            id: SharedString::from("__all__"),
            name: SharedString::from("All Lights"),
            brightness: 0.5,
            warmth: 0.5,
            power_on: any_on,
            is_all: true,
        });

        for state in &states {
            let record = lights.iter().find(|l| l.id == state.id);
            let display_name = record
                .and_then(|r| r.alias.as_deref())
                .unwrap_or_else(|| record.map(|r| r.name.as_str()).unwrap_or("Unknown Light"));
            lights_model.push(LightData {
                id: SharedString::from(state.id.as_str()),
                name: SharedString::from(display_name),
                brightness: api_to_brightness(state.brightness),
                warmth: kelvin_to_warmth(state.kelvin),
                power_on: state.on,
                is_all: false,
            });
        }
    }
    ui.set_lights_model(ModelRc::from(lights_model.clone()));

    // Build initial groups model
    rebuild_groups_model(&ui, &groups, &states);
    drop(groups);

    // ---- Nav tab callbacks (sync power state on tab switch) ----

    ui.on_nav_lights({
        let api = fetch_api.clone();
        let ui_weak = ui.as_weak();
        move || {
            let api = api.clone();
            let ui_weak = ui_weak.clone();
            std::thread::spawn(move || {
                let states = api.get_light_states().unwrap_or_default();
                slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_weak.upgrade() else { return };
                    let model = ui.get_lights_model();
                    for state in &states {
                        for i in 1..model.row_count() {
                            if let Some(mut d) = model.row_data(i) {
                                if d.id.as_str() == state.id {
                                    d.brightness = api_to_brightness(state.brightness);
                                    d.warmth = kelvin_to_warmth(state.kelvin);
                                    d.power_on = state.on;
                                    model.set_row_data(i, d);
                                    break;
                                }
                            }
                        }
                    }
                    // Sync ALL card power from updated individual states
                    let any_on = (1..model.row_count()).any(|i| {
                        model.row_data(i).map(|d| d.power_on).unwrap_or(false)
                    });
                    if let Some(mut all) = model.row_data(0) {
                        all.power_on = any_on;
                        model.set_row_data(0, all);
                    }
                })
                .ok();
            });
        }
    });

    ui.on_nav_groups({
        let api = fetch_api.clone();
        let ui_weak = ui.as_weak();
        move || {
            let api = api.clone();
            let ui_weak = ui_weak.clone();
            std::thread::spawn(move || {
                let groups = api.get_groups().unwrap_or_default();
                let states = api.get_light_states().unwrap_or_default();
                slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_weak.upgrade() else { return };
                    rebuild_groups_model(&ui, &groups, &states);
                })
                .ok();
            });
        }
    });

    ui.on_nav_settings(|| {});

    // ---- Light callbacks ----

    ui.on_light_brightness_changed({
        let ui_weak = ui.as_weak();
        let tx = cmd_tx.clone();
        move |idx, value| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let model = ui.get_lights_model();
            let idx = idx as usize;
            if let Some(mut data) = model.row_data(idx) {
                data.brightness = value;
                model.set_row_data(idx, data.clone());

                if data.is_all {
                    for i in 1..model.row_count() {
                        if let Some(mut d) = model.row_data(i) {
                            d.brightness = value;
                            model.set_row_data(i, d);
                        }
                    }
                }

                let target = target_for(&data);
                let _ = tx.send(UpdateCommand::SliderDrag {
                    target,
                    payload: UpdatePayload {
                        on: None,
                        brightness: Some(brightness_to_api(value)),
                        kelvin: None,
                    },
                });
            }
        }
    });

    ui.on_light_warmth_changed({
        let ui_weak = ui.as_weak();
        let tx = cmd_tx.clone();
        move |idx, value| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let model = ui.get_lights_model();
            let idx = idx as usize;
            if let Some(mut data) = model.row_data(idx) {
                data.warmth = value;
                model.set_row_data(idx, data.clone());

                if data.is_all {
                    for i in 1..model.row_count() {
                        if let Some(mut d) = model.row_data(i) {
                            d.warmth = value;
                            model.set_row_data(i, d);
                        }
                    }
                }

                let target = target_for(&data);
                let _ = tx.send(UpdateCommand::SliderDrag {
                    target,
                    payload: UpdatePayload {
                        on: None,
                        brightness: None,
                        kelvin: Some(warmth_to_kelvin(value)),
                    },
                });
            }
        }
    });

    ui.on_light_power_toggled({
        let ui_weak = ui.as_weak();
        let tx = cmd_tx.clone();
        move |idx| {
            let Some(ui) = ui_weak.upgrade() else { return };
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
                    sync_all_card_power(&model);
                }

                let target = target_for(&data);
                let _ = tx.send(UpdateCommand::PowerToggle {
                    target,
                    on: new_power,
                });
            }
        }
    });

    ui.on_light_slider_released({
        let ui_weak = ui.as_weak();
        let tx = cmd_tx.clone();
        move |idx| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let model = ui.get_lights_model();
            let idx = idx as usize;
            if let Some(data) = model.row_data(idx) {
                let target = target_for(&data);
                let _ = tx.send(UpdateCommand::SliderRelease {
                    target,
                    payload: UpdatePayload {
                        on: None,
                        brightness: Some(brightness_to_api(data.brightness)),
                        kelvin: Some(warmth_to_kelvin(data.warmth)),
                    },
                });
            }
        }
    });

    // ---- Group callbacks ----

    ui.on_group_brightness_changed({
        let ui_weak = ui.as_weak();
        let tx = cmd_tx.clone();
        move |idx, value| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let model = ui.get_groups_model();
            let idx = idx as usize;
            if let Some(mut data) = model.row_data(idx) {
                data.brightness = value;
                model.set_row_data(idx, data.clone());
                let _ = tx.send(UpdateCommand::SliderDrag {
                    target: UpdateTarget::Group(data.name.to_string()),
                    payload: UpdatePayload {
                        on: None,
                        brightness: Some(brightness_to_api(value)),
                        kelvin: None,
                    },
                });
            }
        }
    });

    ui.on_group_warmth_changed({
        let ui_weak = ui.as_weak();
        let tx = cmd_tx.clone();
        move |idx, value| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let model = ui.get_groups_model();
            let idx = idx as usize;
            if let Some(mut data) = model.row_data(idx) {
                data.warmth = value;
                model.set_row_data(idx, data.clone());
                let _ = tx.send(UpdateCommand::SliderDrag {
                    target: UpdateTarget::Group(data.name.to_string()),
                    payload: UpdatePayload {
                        on: None,
                        brightness: None,
                        kelvin: Some(warmth_to_kelvin(value)),
                    },
                });
            }
        }
    });

    ui.on_group_power_toggled({
        let ui_weak = ui.as_weak();
        let tx = cmd_tx.clone();
        move |idx| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let model = ui.get_groups_model();
            let idx = idx as usize;
            if let Some(mut data) = model.row_data(idx) {
                let new_power = !data.power_on;
                data.power_on = new_power;
                model.set_row_data(idx, data.clone());
                let _ = tx.send(UpdateCommand::PowerToggle {
                    target: UpdateTarget::Group(data.name.to_string()),
                    on: new_power,
                });
            }
        }
    });

    ui.on_group_slider_released({
        let ui_weak = ui.as_weak();
        let tx = cmd_tx.clone();
        move |idx| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let model = ui.get_groups_model();
            let idx = idx as usize;
            if let Some(data) = model.row_data(idx) {
                let _ = tx.send(UpdateCommand::SliderRelease {
                    target: UpdateTarget::Group(data.name.to_string()),
                    payload: UpdatePayload {
                        on: None,
                        brightness: Some(brightness_to_api(data.brightness)),
                        kelvin: Some(warmth_to_kelvin(data.warmth)),
                    },
                });
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
                ui.window().with_winit_window(|w| {
                    w.set_minimized(true);
                });
            }
        }
    });

    ui.on_request_add({
        let api = fetch_api.clone();
        let ui_weak = ui.as_weak();
        move || {
            let api = api.clone();
            let ui_weak = ui_weak.clone();
            std::thread::spawn(move || {
                let lights = api.get_lights().unwrap_or_default();
                let states = api.get_light_states().unwrap_or_default();
                let entries: Vec<ManageLightEntry> = lights
                    .iter()
                    .map(|rec| {
                        let reachable = states.iter().any(|s| s.id == rec.id);
                        ManageLightEntry {
                            id: SharedString::from(rec.id.as_str()),
                            name: SharedString::from(rec.name.as_str()),
                            alias: SharedString::from(rec.alias.as_deref().unwrap_or("")),
                            enabled: rec.enabled,
                            reachable,
                        }
                    })
                    .collect();
                slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_weak.upgrade() else { return };
                    let model = Rc::new(VecModel::from(entries));
                    ui.set_manage_model(ModelRc::from(model));
                })
                .ok();
            });
        }
    });

    ui.on_request_scan({
        let api = fetch_api.clone();
        let ui_weak = ui.as_weak();
        move || {
            let api = api.clone();
            let ui_weak = ui_weak.clone();
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_scanning(true);
            }
            std::thread::spawn(move || {
                let _ = api.refresh_lights();
                let lights = api.get_lights().unwrap_or_default();
                let states = api.get_light_states().unwrap_or_default();
                let entries: Vec<ManageLightEntry> = lights
                    .iter()
                    .map(|rec| {
                        let reachable = states.iter().any(|s| s.id == rec.id);
                        ManageLightEntry {
                            id: SharedString::from(rec.id.as_str()),
                            name: SharedString::from(rec.name.as_str()),
                            alias: SharedString::from(rec.alias.as_deref().unwrap_or("")),
                            enabled: rec.enabled,
                            reachable,
                        }
                    })
                    .collect();
                slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_weak.upgrade() else { return };
                    let model = Rc::new(VecModel::from(entries));
                    ui.set_manage_model(ModelRc::from(model));
                    ui.set_scanning(false);
                })
                .ok();
            });
        }
    });

    ui.on_manage_enable_toggled({
        let api = fetch_api.clone();
        let ui_weak = ui.as_weak();
        move |idx| {
            let ui_weak = ui_weak.clone();
            let api = api.clone();
            let ui = match ui_weak.upgrade() {
                Some(ui) => ui,
                None => return,
            };
            let model = ui.get_manage_model();
            let idx = idx as usize;
            if let Some(mut entry) = model.row_data(idx) {
                let new_enabled = !entry.enabled;
                entry.enabled = new_enabled;
                model.set_row_data(idx, entry.clone());
                let id = entry.id.to_string();
                std::thread::spawn(move || {
                    let _ = api.set_light_enabled(&id, new_enabled);
                    let lights = api.get_lights().unwrap_or_default();
                    let states = api.get_light_states().unwrap_or_default();
                    slint::invoke_from_event_loop(move || {
                        let Some(ui) = ui_weak.upgrade() else { return };
                        rebuild_lights_model(&ui, &lights, &states);
                    })
                    .ok();
                });
            }
        }
    });

    ui.on_manage_rename({
        let api = fetch_api.clone();
        let ui_weak = ui.as_weak();
        move |idx, new_name| {
            let api = api.clone();
            let ui = match ui_weak.upgrade() {
                Some(ui) => ui,
                None => return,
            };
            let model = ui.get_manage_model();
            let idx = idx as usize;
            if let Some(mut entry) = model.row_data(idx) {
                entry.alias = new_name.clone();
                model.set_row_data(idx, entry.clone());
                let id = entry.id.to_string();
                let alias = new_name.to_string();
                std::thread::spawn(move || {
                    let _ = api.set_light_alias(&id, &alias);
                });
            }
        }
    });

    ui.on_manage_delete({
        let api = fetch_api.clone();
        let ui_weak = ui.as_weak();
        move |idx| {
            let ui_weak = ui_weak.clone();
            let api = api.clone();
            let ui = match ui_weak.upgrade() {
                Some(ui) => ui,
                None => return,
            };
            let manage = ui.get_manage_model();
            let idx = idx as usize;
            if let Some(entry) = manage.row_data(idx) {
                // Remove from manage model immediately
                let mut entries: Vec<ManageLightEntry> = Vec::new();
                for i in 0..manage.row_count() {
                    if i != idx {
                        if let Some(e) = manage.row_data(i) {
                            entries.push(e);
                        }
                    }
                }
                ui.set_manage_model(ModelRc::from(Rc::new(VecModel::from(entries))));

                // Also remove from lights model immediately
                let lmodel = ui.get_lights_model();
                let light_id = entry.id.clone();
                let mut light_entries: Vec<LightData> = Vec::new();
                for i in 0..lmodel.row_count() {
                    if let Some(d) = lmodel.row_data(i) {
                        if d.id != light_id {
                            light_entries.push(d);
                        }
                    }
                }
                ui.set_lights_model(ModelRc::from(Rc::new(VecModel::from(light_entries))));

                let id = entry.id.to_string();
                std::thread::spawn(move || {
                    if let Err(e) = api.delete_light(&id) {
                        eprintln!("delete_light failed: {e}");
                    }
                });
            }
        }
    });

    ui.on_manage_close({
        let api = fetch_api.clone();
        let ui_weak = ui.as_weak();
        move || {
            let api = api.clone();
            let ui_weak = ui_weak.clone();
            std::thread::spawn(move || {
                let lights = api.get_lights().unwrap_or_default();
                let states = api.get_light_states().unwrap_or_default();
                slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_weak.upgrade() else { return };
                    rebuild_lights_model(&ui, &lights, &states);
                })
                .ok();
            });
        }
    });

    // ---- Manage groups panel callbacks ----

    ui.on_request_groups_panel({
        let api = fetch_api.clone();
        let ui_weak = ui.as_weak();
        move || {
            let api = api.clone();
            let ui_weak = ui_weak.clone();
            std::thread::spawn(move || {
                let lights = api.get_lights().unwrap_or_default();
                let states = api.get_light_states().unwrap_or_default();
                let picks: Vec<GroupLightPick> = lights
                    .iter()
                    .filter(|l| l.enabled && states.iter().any(|s| s.id == l.id))
                    .map(|l| GroupLightPick {
                        id: SharedString::from(l.id.as_str()),
                        name: SharedString::from(
                            l.alias.as_deref().unwrap_or(l.name.as_str()),
                        ),
                        selected: false,
                    })
                    .collect();
                slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_weak.upgrade() else { return };
                    ui.set_group_lights_pick(ModelRc::from(
                        Rc::new(VecModel::from(picks)),
                    ));
                })
                .ok();
            });
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
        let api = fetch_api.clone();
        let ui_weak = ui.as_weak();
        move |name| {
            let name = name.to_string();
            if name.trim().is_empty() {
                return;
            }
            let ui = match ui_weak.upgrade() {
                Some(ui) => ui,
                None => return,
            };
            let pick_model = ui.get_group_lights_pick();
            let members: Vec<String> = (0..pick_model.row_count())
                .filter_map(|i| {
                    let entry = pick_model.row_data(i)?;
                    if entry.selected {
                        Some(entry.id.to_string())
                    } else {
                        None
                    }
                })
                .collect();
            if members.is_empty() {
                return;
            }
            let api = api.clone();
            let ui_weak = ui_weak.clone();
            std::thread::spawn(move || {
                if let Err(e) = api.create_group(&name, &members) {
                    eprintln!("create_group failed: {e}");
                    return;
                }
                let groups = api.get_groups().unwrap_or_default();
                let states = api.get_light_states().unwrap_or_default();
                slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_weak.upgrade() else { return };
                    rebuild_groups_model(&ui, &groups, &states);
                    ui.set_groups_panel_open(false);
                })
                .ok();
            });
        }
    });

    ui.on_group_manage_delete({
        let api = fetch_api.clone();
        let ui_weak = ui.as_weak();
        move |idx| {
            let ui = match ui_weak.upgrade() {
                Some(ui) => ui,
                None => return,
            };
            let model = ui.get_groups_model();
            let idx = idx as usize;
            if let Some(data) = model.row_data(idx) {
                let name = data.name.to_string();
                let mut entries: Vec<GroupData> = Vec::new();
                for i in 0..model.row_count() {
                    if i != idx {
                        if let Some(d) = model.row_data(i) {
                            entries.push(d);
                        }
                    }
                }
                ui.set_groups_model(ModelRc::from(Rc::new(VecModel::from(entries))));

                let api = api.clone();
                std::thread::spawn(move || {
                    if let Err(e) = api.delete_group(&name) {
                        eprintln!("delete_group failed: {e}");
                    }
                });
            }
        }
    });

    ui.on_group_manage_close({
        let api = fetch_api.clone();
        let ui_weak = ui.as_weak();
        move || {
            let api = api.clone();
            let ui_weak = ui_weak.clone();
            std::thread::spawn(move || {
                let groups = api.get_groups().unwrap_or_default();
                let states = api.get_light_states().unwrap_or_default();
                slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_weak.upgrade() else { return };
                    rebuild_groups_model(&ui, &groups, &states);
                })
                .ok();
            });
        }
    });

    // ---- Settings: autostart ----

    ui.set_autostart_enabled(autostart_desktop_exists());

    ui.on_autostart_toggled({
        let ui_weak = ui.as_weak();
        move |enabled| {
            if enabled {
                write_autostart_desktop();
            } else {
                remove_autostart_desktop();
            }
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_autostart_enabled(autostart_desktop_exists());
            }
        }
    });

    let is_wayland = std::env::var("WAYLAND_DISPLAY").is_ok();
    let drag_in_progress = Rc::new(RefCell::new(false));
    let cursor_left_during_drag = Rc::new(RefCell::new(false));

    ui.window().on_winit_window_event({
        let drag_in_progress = Rc::clone(&drag_in_progress);
        let cursor_left_during_drag = Rc::clone(&cursor_left_during_drag);
        let ui_weak = ui.as_weak();
        move |_w, event| {
            match event {
                WindowEvent::MouseInput { .. } => {}
                WindowEvent::Touch { .. } => {}
                WindowEvent::CursorMoved { .. } => {
                    if *drag_in_progress.borrow() {
                        *drag_in_progress.borrow_mut() = false;
                        *cursor_left_during_drag.borrow_mut() = false;
                        if let Some(ui) = ui_weak.upgrade() {
                            ui.invoke_reset_drag_state();
                        }
                    }
                }
                WindowEvent::CursorEntered { .. } => {
                    if *drag_in_progress.borrow() && *cursor_left_during_drag.borrow() {
                        *drag_in_progress.borrow_mut() = false;
                        *cursor_left_during_drag.borrow_mut() = false;
                        if let Some(ui) = ui_weak.upgrade() {
                            ui.invoke_reset_drag_state();
                        }
                    }
                }
                WindowEvent::CursorLeft { .. } => {
                    if *drag_in_progress.borrow() {
                        *cursor_left_during_drag.borrow_mut() = true;
                    }
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
            if is_wayland {
                return;
            }
            if dx == 0 && dy == 0 {
                return;
            }
            if let Some(ui) = ui_weak.upgrade() {
                ui.window().with_winit_window(|w| {
                    if let Ok(pos) = w.outer_position() {
                        let new_pos =
                            winit::dpi::PhysicalPosition::new(pos.x + dx, pos.y + dy);
                        w.set_outer_position(new_pos);
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

/// If ANY individual light is on, set the ALL card to on; otherwise off.
fn sync_all_card_power(model: &ModelRc<LightData>) {
    let any_on = (1..model.row_count()).any(|i| {
        model.row_data(i).map(|d| d.power_on).unwrap_or(false)
    });
    if let Some(mut all) = model.row_data(0) {
        if all.is_all && all.power_on != any_on {
            all.power_on = any_on;
            model.set_row_data(0, all);
        }
    }
}

/// Rebuild the main lights model from fresh API data (ALL card + enabled & reachable lights).
fn rebuild_lights_model(
    ui: &MainWindow,
    lights: &[LightRecord],
    states: &[api::LightStateResponse],
) {
    let any_on = states.iter().any(|s| {
        s.on && lights.iter().any(|l| l.id == s.id && l.enabled)
    });
    let mut entries = vec![LightData {
        id: SharedString::from("__all__"),
        name: SharedString::from("All Lights"),
        brightness: 0.5,
        warmth: 0.5,
        power_on: any_on,
        is_all: true,
    }];
    for state in states {
        let record = lights.iter().find(|l| l.id == state.id);
        let is_enabled = record.map(|r| r.enabled).unwrap_or(false);
        if !is_enabled {
            continue;
        }
        let display_name = record
            .and_then(|r| r.alias.as_deref())
            .unwrap_or_else(|| record.map(|r| r.name.as_str()).unwrap_or("Unknown Light"));
        entries.push(LightData {
            id: SharedString::from(state.id.as_str()),
            name: SharedString::from(display_name),
            brightness: api_to_brightness(state.brightness),
            warmth: kelvin_to_warmth(state.kelvin),
            power_on: state.on,
            is_all: false,
        });
    }
    ui.set_lights_model(ModelRc::from(Rc::new(VecModel::from(entries))));
}

/// Rebuild the groups model from fresh API data, syncing power state per group.
fn rebuild_groups_model(
    ui: &MainWindow,
    groups: &[GroupRecord],
    states: &[api::LightStateResponse],
) {
    let entries: Vec<GroupData> = groups
        .iter()
        .map(|g| {
            let any_on = g.members.iter().any(|mid| {
                states.iter().any(|s| s.id == *mid && s.on)
            });
            GroupData {
                name: SharedString::from(g.name.as_str()),
                brightness: 0.5,
                warmth: 0.5,
                power_on: any_on,
            }
        })
        .collect();
    ui.set_groups_model(ModelRc::from(Rc::new(VecModel::from(entries))));
}

// ---- Autostart (.desktop file in ~/.config/autostart/) ----

const FLATPAK_APP_ID: &str = "io.github.chimi6.limelight-linux-elgato-lights-controller";

fn is_flatpak() -> bool {
    std::path::Path::new("/.flatpak-info").exists()
}

fn autostart_dir() -> std::path::PathBuf {
    if is_flatpak() {
        // Inside Flatpak, XDG_CONFIG_HOME is sandboxed — but autostart
        // needs to live on the host at ~/.config/autostart/.
        // Flatpak exposes the host home via /var/home or ~/.
        dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("/var/home"))
            .join(".config/autostart")
    } else {
        dirs::config_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("~/.config"))
            .join("autostart")
    }
}

fn autostart_path() -> std::path::PathBuf {
    autostart_dir().join(format!("{FLATPAK_APP_ID}.desktop"))
}

fn autostart_desktop_exists() -> bool {
    autostart_path().exists()
}

fn autostart_exec_line() -> String {
    if is_flatpak() {
        format!("flatpak run {FLATPAK_APP_ID}")
    } else {
        std::env::current_exe()
            .unwrap_or_else(|_| std::path::PathBuf::from("keylight-gui"))
            .display()
            .to_string()
    }
}

fn write_autostart_desktop() {
    let dir = autostart_dir();
    let _ = std::fs::create_dir_all(&dir);
    let contents = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=LimeLight\n\
         Comment=Elgato Key Light Controller\n\
         Exec={}\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n",
        autostart_exec_line()
    );
    if let Err(e) = std::fs::write(autostart_path(), contents) {
        eprintln!("failed to write autostart desktop file: {e}");
    }
}

fn remove_autostart_desktop() {
    if let Err(e) = std::fs::remove_file(autostart_path()) {
        if e.kind() != std::io::ErrorKind::NotFound {
            eprintln!("failed to remove autostart desktop file: {e}");
        }
    }
}

/// If the keylightd daemon isn't reachable, spawn it in the background and wait briefly.
fn ensure_daemon_running() {
    let probe = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_millis(500))
        .build()
        .ok()
        .and_then(|c| c.get("http://127.0.0.1:9124/v1/lights").send().ok());

    if probe.is_some() {
        return;
    }

    eprintln!("keylightd not reachable, attempting to start…");

    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));

    let daemon_path = exe_dir
        .as_ref()
        .map(|d| d.join("keylightd"))
        .filter(|p| p.exists())
        .unwrap_or_else(|| std::path::PathBuf::from("keylightd"));

    match std::process::Command::new(&daemon_path)
        .arg("serve")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_) => {
            // Give the daemon a moment to bind its port
            for _ in 0..10 {
                std::thread::sleep(std::time::Duration::from_millis(300));
                let ok = reqwest::blocking::Client::builder()
                    .timeout(std::time::Duration::from_millis(500))
                    .build()
                    .ok()
                    .and_then(|c| c.get("http://127.0.0.1:9124/v1/lights").send().ok());
                if ok.is_some() {
                    eprintln!("keylightd is now running");
                    return;
                }
            }
            eprintln!("keylightd spawned but still not reachable — continuing anyway");
        }
        Err(e) => {
            eprintln!("failed to start keylightd: {e}");
        }
    }
}
