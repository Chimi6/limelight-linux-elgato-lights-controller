use eframe::egui;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

const DEFAULT_API_URL: &str = "http://127.0.0.1:9124";

#[derive(Clone, Debug, Deserialize)]
struct LightRecord {
    id: String,
    alias: Option<String>,
    name: String,
    enabled: bool,
}

#[derive(Clone, Debug, Deserialize)]
struct GroupRecord {
    name: String,
    members: Vec<String>,
}

#[derive(Serialize, Clone)]
struct UpdateRequest {
    on: Option<u8>,
    brightness: Option<u8>,
    kelvin: Option<u16>,
    mired: Option<u16>,
}

#[derive(Serialize)]
struct GroupRequest {
    name: String,
    members: Vec<String>,
}

#[derive(Clone)]
struct LightControl {
    id: String,
    label: String,
    enabled: bool,
    on: bool,
    brightness: u8,
    kelvin: u16,
}

struct GroupControl {
    brightness: u8,
    kelvin: u16,
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Lights,
    Groups,
}

#[derive(PartialEq, Clone, Copy)]
enum ModalState {
    None,
    Discover,
    CreateGroup,
}

struct KeylightApp {
    client: Client,
    api_url: String,
    lights: Vec<LightControl>,
    groups: Vec<GroupRecord>,
    group_controls: HashMap<String, GroupControl>,
    active_tab: Tab,
    modal_state: ModalState,
    new_group_name: String,
    new_group_members: HashSet<String>,
    last_sent: HashMap<String, std::time::Instant>,
    last_error: Option<String>,
    logo: Option<egui::TextureHandle>,
    all_brightness: u8,
    all_kelvin: u16,
}

impl KeylightApp {
    fn new() -> Self {
        let api_url = std::env::var("KEYLIGHT_API_URL").unwrap_or_else(|_| DEFAULT_API_URL.into());
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let mut app = Self {
            client,
            api_url,
            lights: Vec::new(),
            groups: Vec::new(),
            group_controls: HashMap::new(),
            active_tab: Tab::Lights,
            modal_state: ModalState::None,
            new_group_name: String::new(),
            new_group_members: HashSet::new(),
            last_sent: HashMap::new(),
            last_error: None,
            logo: None,
            all_brightness: 50,
            all_kelvin: 4500,
        };
        app.refresh_all();
        app
    }

    fn refresh_all(&mut self) {
        self.refresh_lights();
        self.refresh_groups();
    }

    fn refresh_lights(&mut self) {
        let url = format!("{}/v1/lights", self.api_url);
        match self
            .client
            .get(url)
            .send()
            .and_then(|res| res.error_for_status())
        {
            Ok(res) => match res.json::<Vec<LightRecord>>() {
                Ok(records) => {
                    let mut updated = Vec::new();
                    for record in records {
                        let label = record.alias.clone().unwrap_or_else(|| {
                            // Shorten the mDNS name for display
                            record
                                .name
                                .split('.')
                                .next()
                                .unwrap_or(&record.name)
                                .to_string()
                        });
                        let previous = self
                            .lights
                            .iter()
                            .find(|item| item.id == record.id)
                            .cloned();
                        updated.push(LightControl {
                            id: record.id.clone(),
                            label,
                            enabled: record.enabled,
                            on: previous.as_ref().map(|p| p.on).unwrap_or(true),
                            brightness: previous.as_ref().map(|p| p.brightness).unwrap_or(50),
                            kelvin: previous.as_ref().map(|p| p.kelvin).unwrap_or(4500),
                        });
                    }
                    self.lights = updated;
                    self.new_group_members
                        .retain(|id| self.lights.iter().any(|light| &light.id == id));
                    self.last_error = None;
                }
                Err(err) => self.last_error = Some(format!("Parse error: {}", err)),
            },
            Err(err) => self.last_error = Some(format!("Connection error: {}", err)),
        }
    }

    fn refresh_groups(&mut self) {
        let url = format!("{}/v1/groups", self.api_url);
        match self
            .client
            .get(url)
            .send()
            .and_then(|res| res.error_for_status())
        {
            Ok(res) => match res.json::<Vec<GroupRecord>>() {
                Ok(groups) => {
                    for group in &groups {
                        self.group_controls
                            .entry(group.name.clone())
                            .or_insert(GroupControl {
                                brightness: 50,
                                kelvin: 4500,
                            });
                    }
                    self.groups = groups;
                }
                Err(err) => self.last_error = Some(format!("Parse error: {}", err)),
            },
            Err(err) => self.last_error = Some(format!("Connection error: {}", err)),
        }
    }

    fn save_group(&mut self, name: String, members: Vec<String>) {
        let url = format!("{}/v1/groups", self.api_url);
        let result = self
            .client
            .post(url)
            .json(&GroupRequest { name, members })
            .send()
            .and_then(|res| res.error_for_status());
        if let Err(err) = result {
            self.last_error = Some(err.to_string());
        } else {
            self.last_error = None;
            self.refresh_groups();
        }
    }

    fn delete_group(&mut self, name: &str) {
        let encoded = urlencoding::encode(name);
        let url = format!("{}/v1/groups/{}", self.api_url, encoded);
        let result = self
            .client
            .delete(url)
            .send()
            .and_then(|res| res.error_for_status());
        if let Err(err) = result {
            self.last_error = Some(err.to_string());
        } else {
            self.last_error = None;
            self.group_controls.remove(name);
            self.refresh_groups();
        }
    }

    fn send_light_update(&mut self, id: &str, update: UpdateRequest) {
        let encoded = urlencoding::encode(id);
        let url = format!("{}/v1/lights/{}", self.api_url, encoded);
        let result = self
            .client
            .put(url)
            .json(&update)
            .send()
            .and_then(|res| res.error_for_status());
        if let Err(err) = result {
            self.last_error = Some(err.to_string());
        } else {
            self.last_error = None;
        }
    }

    fn send_group_update(&mut self, name: &str, update: UpdateRequest) {
        let encoded = urlencoding::encode(name);
        let url = format!("{}/v1/groups/{}", self.api_url, encoded);
        let result = self
            .client
            .put(url)
            .json(&update)
            .send()
            .and_then(|res| res.error_for_status());
        if let Err(err) = result {
            self.last_error = Some(err.to_string());
        } else {
            self.last_error = None;
        }
    }

    fn send_all_update(&mut self, update: UpdateRequest) {
        let url = format!("{}/v1/all", self.api_url);
        let result = self
            .client
            .put(url)
            .json(&update)
            .send()
            .and_then(|res| res.error_for_status());
        if let Err(err) = result {
            self.last_error = Some(err.to_string());
        } else {
            self.last_error = None;
        }
    }

    fn refresh_discovery(&mut self) {
        let url = format!("{}/v1/lights/refresh", self.api_url);
        let result = self
            .client
            .post(url)
            .json(&serde_json::json!({"timeout": 3}))
            .send()
            .and_then(|res| res.error_for_status());
        if let Err(err) = result {
            self.last_error = Some(err.to_string());
        } else {
            self.last_error = None;
            self.refresh_lights();
        }
    }

    fn set_light_enabled(&mut self, id: &str, enabled: bool) {
        let encoded = urlencoding::encode(id);
        let url = format!("{}/v1/lights/{}/enabled", self.api_url, encoded);
        let result = self
            .client
            .put(url)
            .json(&serde_json::json!({ "enabled": enabled }))
            .send()
            .and_then(|res| res.error_for_status());
        if let Err(err) = result {
            self.last_error = Some(err.to_string());
        } else {
            self.last_error = None;
        }
    }

    fn should_send(&mut self, key: &str) -> bool {
        let now = std::time::Instant::now();
        let interval = std::time::Duration::from_millis(100);
        match self.last_sent.get(key) {
            Some(last) if now.duration_since(*last) < interval => false,
            _ => {
                self.last_sent.insert(key.to_string(), now);
                true
            }
        }
    }

    fn ensure_logo(&mut self, ctx: &egui::Context) {
        if self.logo.is_some() {
            return;
        }
        let bytes = include_bytes!("../../../../public/Limecon.png");
        if let Ok(image) = image::load_from_memory(bytes) {
            let rgba = image.to_rgba8();
            let size = [rgba.width() as usize, rgba.height() as usize];
            let pixels = rgba.into_raw();
            let color_image = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
            self.logo =
                Some(ctx.load_texture("limecon", color_image, egui::TextureOptions::LINEAR));
        }
    }
}

impl eframe::App for KeylightApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ensure_logo(ctx);

        // Frutiger Aero styling
        let mut visuals = egui::Visuals::light();
        visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(235, 245, 255);
        visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(215, 235, 255);
        visuals.widgets.active.bg_fill = egui::Color32::from_rgb(190, 220, 255);
        visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(245, 250, 255);
        visuals.window_rounding = egui::Rounding::same(10.0);
        visuals.window_shadow = egui::Shadow {
            offset: egui::vec2(0.0, 4.0),
            blur: 12.0,
            spread: 0.0,
            color: egui::Color32::from_rgba_premultiplied(40, 80, 120, 60),
        };
        ctx.set_visuals(visuals);

        // Collect pending actions
        let mut pending_light_updates: Vec<(String, UpdateRequest)> = Vec::new();
        let mut pending_group_updates: Vec<(String, UpdateRequest)> = Vec::new();
        let mut pending_all_update: Option<UpdateRequest> = None;
        let mut pending_enabled: Vec<(String, bool)> = Vec::new();
        let mut pending_delete_groups: Vec<String> = Vec::new();

        // Header
        egui::TopBottomPanel::top("header")
            .exact_height(48.0)
            .show(ctx, |ui| {
                egui::Frame::none()
                    .fill(egui::Color32::from_rgb(220, 240, 255))
                    .inner_margin(egui::Margin::symmetric(8.0, 6.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if let Some(logo) = &self.logo {
                                ui.add(
                                    egui::Image::new(logo).fit_to_exact_size(egui::vec2(28.0, 28.0)),
                                );
                            }
                            ui.label(
                                egui::RichText::new("Limekit")
                                    .size(16.0)
                                    .strong()
                                    .color(egui::Color32::from_rgb(40, 80, 130)),
                            );
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui
                                    .button(egui::RichText::new("⟳").size(16.0))
                                    .on_hover_text("Refresh")
                                    .clicked()
                                {
                                    self.refresh_all();
                                }
                            });
                        });
                    });
            });

        // Tab bar
        egui::TopBottomPanel::top("tabs")
            .exact_height(32.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(4.0);
                    let lights_tab = ui.selectable_label(
                        self.active_tab == Tab::Lights,
                        egui::RichText::new("💡 Lights").size(13.0),
                    );
                    if lights_tab.clicked() {
                        self.active_tab = Tab::Lights;
                        self.modal_state = ModalState::None;
                    }

                    let groups_tab = ui.selectable_label(
                        self.active_tab == Tab::Groups,
                        egui::RichText::new("📁 Groups").size(13.0),
                    );
                    if groups_tab.clicked() {
                        self.active_tab = Tab::Groups;
                        self.modal_state = ModalState::None;
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let plus_text = match self.active_tab {
                            Tab::Lights => "＋",
                            Tab::Groups => "＋",
                        };
                        let plus_hint = match self.active_tab {
                            Tab::Lights => "Discover lights",
                            Tab::Groups => "Create group",
                        };
                        if ui
                            .button(egui::RichText::new(plus_text).size(14.0))
                            .on_hover_text(plus_hint)
                            .clicked()
                        {
                            self.modal_state = match self.active_tab {
                                Tab::Lights => {
                                    if self.modal_state == ModalState::Discover {
                                        ModalState::None
                                    } else {
                                        ModalState::Discover
                                    }
                                }
                                Tab::Groups => {
                                    if self.modal_state == ModalState::CreateGroup {
                                        ModalState::None
                                    } else {
                                        self.new_group_name.clear();
                                        self.new_group_members.clear();
                                        ModalState::CreateGroup
                                    }
                                }
                            };
                        }
                        ui.add_space(4.0);
                    });
                });
            });

        // Error banner (if any)
        if self.last_error.is_some() {
            egui::TopBottomPanel::top("error")
                .exact_height(24.0)
                .show(ctx, |ui| {
                    egui::Frame::none()
                        .fill(egui::Color32::from_rgb(255, 230, 230))
                        .inner_margin(egui::Margin::symmetric(8.0, 4.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                if let Some(err) = &self.last_error {
                                    ui.label(
                                        egui::RichText::new(format!("⚠ {}", err))
                                            .size(11.0)
                                            .color(egui::Color32::from_rgb(180, 60, 60)),
                                    );
                                }
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.small_button("✕").clicked() {
                                            self.last_error = None;
                                        }
                                    },
                                );
                            });
                        });
                });
        }

        // Main content
        egui::CentralPanel::default().show(ctx, |ui| {
            // Background gradient
            let bg = ui.max_rect();
            ui.painter()
                .rect_filled(bg, 0.0, egui::Color32::from_rgb(248, 252, 255));

            match self.active_tab {
                Tab::Lights => {
                    // Modal: Discover lights
                    if self.modal_state == ModalState::Discover {
                        egui::Frame::none()
                            .fill(egui::Color32::from_rgb(240, 248, 255))
                            .stroke(egui::Stroke::new(
                                1.0,
                                egui::Color32::from_rgb(180, 210, 240),
                            ))
                            .rounding(egui::Rounding::same(8.0))
                            .inner_margin(egui::Margin::same(10.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new("🔍 Discover Lights")
                                            .size(13.0)
                                            .strong(),
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui.small_button("✕").clicked() {
                                                self.modal_state = ModalState::None;
                                            }
                                        },
                                    );
                                });
                                ui.add_space(4.0);
                                ui.label("Scan network for Elgato lights:");
                                ui.add_space(4.0);
                                if ui.button("Scan Now").clicked() {
                                    self.refresh_discovery();
                                    self.modal_state = ModalState::None;
                                }
                                ui.add_space(6.0);
                                ui.separator();
                                ui.label(
                                    egui::RichText::new("Manage discovered lights:")
                                        .size(11.0)
                                        .weak(),
                                );
                                for light in &mut self.lights {
                                    let mut enabled = light.enabled;
                                    if ui.checkbox(&mut enabled, &light.label).changed() {
                                        light.enabled = enabled;
                                        pending_enabled.push((light.id.clone(), enabled));
                                    }
                                }
                            });
                        ui.add_space(8.0);
                    }

                    // All lights control
                    egui::Frame::none()
                        .fill(egui::Color32::from_rgb(230, 244, 255))
                        .stroke(egui::Stroke::new(
                            1.0,
                            egui::Color32::from_rgb(190, 220, 250),
                        ))
                        .rounding(egui::Rounding::same(8.0))
                        .inner_margin(egui::Margin::same(10.0))
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new("All Lights").size(12.0).strong());
                            ui.add_space(4.0);
                            let mut brightness = self.all_brightness;
                            let mut kelvin = self.all_kelvin;
                            let b_changed = ui
                                .add(
                                    egui::Slider::new(&mut brightness, 0..=100)
                                        .text("☀")
                                        .show_value(true),
                                )
                                .changed();
                            let k_changed = ui
                                .add(
                                    egui::Slider::new(&mut kelvin, 2900..=7000)
                                        .text("🌡")
                                        .show_value(true),
                                )
                                .changed();
                            self.all_brightness = brightness;
                            self.all_kelvin = kelvin;
                            if (b_changed || k_changed) && self.should_send("all") {
                                pending_all_update = Some(UpdateRequest {
                                    on: None,
                                    brightness: Some(brightness),
                                    kelvin: Some(kelvin),
                                    mired: None,
                                });
                            }
                        });

                    ui.add_space(8.0);

                    // Individual lights
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        let light_count = self.lights.len();
                        for index in 0..light_count {
                            if !self.lights[index].enabled {
                                continue;
                            }

                            let (light_id, label, mut on, mut brightness, mut kelvin) = {
                                let l = &self.lights[index];
                                (l.id.clone(), l.label.clone(), l.on, l.brightness, l.kelvin)
                            };

                            let mut changed = false;
                            let mut power_changed = false;

                            egui::Frame::none()
                                .fill(egui::Color32::from_rgb(242, 250, 255))
                                .stroke(egui::Stroke::new(
                                    1.0,
                                    egui::Color32::from_rgb(200, 225, 250),
                                ))
                                .rounding(egui::Rounding::same(8.0))
                                .inner_margin(egui::Margin::same(8.0))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        // Power toggle
                                        let power_icon = if on { "🌕" } else { "🌑" };
                                        let power_btn = ui.button(
                                            egui::RichText::new(power_icon)
                                                .size(16.0),
                                        );
                                        if power_btn.clicked() {
                                            on = !on;
                                            power_changed = true;
                                        }
                                        power_btn.on_hover_text(if on {
                                            "Turn off"
                                        } else {
                                            "Turn on"
                                        });

                                        ui.label(
                                            egui::RichText::new(&label).size(12.0).strong(),
                                        );
                                    });

                                    let b_changed = ui
                                        .add(
                                            egui::Slider::new(&mut brightness, 0..=100)
                                                .text("☀")
                                                .show_value(true),
                                        )
                                        .changed();
                                    let k_changed = ui
                                        .add(
                                            egui::Slider::new(&mut kelvin, 2900..=7000)
                                                .text("🌡")
                                                .show_value(true),
                                        )
                                        .changed();
                                    changed = b_changed || k_changed;
                                });

                            {
                                let l = &mut self.lights[index];
                                l.on = on;
                                l.brightness = brightness;
                                l.kelvin = kelvin;
                            }

                            if power_changed {
                                pending_light_updates.push((
                                    light_id.clone(),
                                    UpdateRequest {
                                        on: Some(if on { 1 } else { 0 }),
                                        brightness: None,
                                        kelvin: None,
                                        mired: None,
                                    },
                                ));
                            }

                            if changed {
                                let key = format!("light:{}", light_id);
                                if self.should_send(&key) {
                                    pending_light_updates.push((
                                        light_id.clone(),
                                        UpdateRequest {
                                            on: None,
                                            brightness: Some(brightness),
                                            kelvin: Some(kelvin),
                                            mired: None,
                                        },
                                    ));
                                }
                            }

                            ui.add_space(4.0);
                        }

                        // Empty state
                        let enabled_count = self.lights.iter().filter(|l| l.enabled).count();
                        if enabled_count == 0 {
                            ui.add_space(20.0);
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    egui::RichText::new("No lights enabled")
                                        .size(12.0)
                                        .weak(),
                                );
                                ui.label(
                                    egui::RichText::new("Click ＋ to discover lights")
                                        .size(11.0)
                                        .weak(),
                                );
                            });
                        }
                    });
                }

                Tab::Groups => {
                    // Modal: Create group
                    if self.modal_state == ModalState::CreateGroup {
                        egui::Frame::none()
                            .fill(egui::Color32::from_rgb(240, 248, 255))
                            .stroke(egui::Stroke::new(
                                1.0,
                                egui::Color32::from_rgb(180, 210, 240),
                            ))
                            .rounding(egui::Rounding::same(8.0))
                            .inner_margin(egui::Margin::same(10.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new("📁 Create Group").size(13.0).strong(),
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui.small_button("✕").clicked() {
                                                self.modal_state = ModalState::None;
                                            }
                                        },
                                    );
                                });
                                ui.add_space(4.0);
                                ui.label("Group name:");
                                ui.text_edit_singleline(&mut self.new_group_name);
                                ui.add_space(4.0);
                                ui.label("Select members:");
                                for light in &self.lights {
                                    let mut selected = self.new_group_members.contains(&light.id);
                                    if ui.checkbox(&mut selected, &light.label).changed() {
                                        if selected {
                                            self.new_group_members.insert(light.id.clone());
                                        } else {
                                            self.new_group_members.remove(&light.id);
                                        }
                                    }
                                }
                                ui.add_space(6.0);
                                let can_save = !self.new_group_name.trim().is_empty()
                                    && !self.new_group_members.is_empty();
                                ui.add_enabled_ui(can_save, |ui| {
                                    if ui.button("Save Group").clicked() {
                                        let name = self.new_group_name.trim().to_string();
                                        let members: Vec<_> =
                                            self.new_group_members.iter().cloned().collect();
                                        self.save_group(name, members);
                                        self.new_group_name.clear();
                                        self.new_group_members.clear();
                                        self.modal_state = ModalState::None;
                                    }
                                });
                            });
                        ui.add_space(8.0);
                    }

                    // Groups list
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        let groups = self.groups.clone();
                        for group in groups {
                            let control = self
                                .group_controls
                                .entry(group.name.clone())
                                .or_insert(GroupControl {
                                    brightness: 50,
                                    kelvin: 4500,
                                });
                            let mut brightness = control.brightness;
                            let mut kelvin = control.kelvin;
                            let name = group.name.clone();
                            let mut changed = false;
                            let mut delete_clicked = false;

                            egui::Frame::none()
                                .fill(egui::Color32::from_rgb(238, 248, 255))
                                .stroke(egui::Stroke::new(
                                    1.0,
                                    egui::Color32::from_rgb(195, 225, 250),
                                ))
                                .rounding(egui::Rounding::same(8.0))
                                .inner_margin(egui::Margin::same(10.0))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new(&name).size(12.0).strong(),
                                        );
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "({} lights)",
                                                group.members.len()
                                            ))
                                            .size(10.0)
                                            .weak(),
                                        );
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if ui
                                                    .button(egui::RichText::new("🗑").size(12.0))
                                                    .on_hover_text("Delete group")
                                                    .clicked()
                                                {
                                                    delete_clicked = true;
                                                }
                                            },
                                        );
                                    });

                                    let b_changed = ui
                                        .add(
                                            egui::Slider::new(&mut brightness, 0..=100)
                                                .text("☀")
                                                .show_value(true),
                                        )
                                        .changed();
                                    let k_changed = ui
                                        .add(
                                            egui::Slider::new(&mut kelvin, 2900..=7000)
                                                .text("🌡")
                                                .show_value(true),
                                        )
                                        .changed();
                                    changed = b_changed || k_changed;
                                });

                            if let Some(ctrl) = self.group_controls.get_mut(&name) {
                                ctrl.brightness = brightness;
                                ctrl.kelvin = kelvin;
                            }

                            if changed {
                                let key = format!("group:{}", name);
                                if self.should_send(&key) {
                                    pending_group_updates.push((
                                        name.clone(),
                                        UpdateRequest {
                                            on: None,
                                            brightness: Some(brightness),
                                            kelvin: Some(kelvin),
                                            mired: None,
                                        },
                                    ));
                                }
                            }

                            if delete_clicked {
                                pending_delete_groups.push(name.clone());
                            }

                            ui.add_space(4.0);
                        }

                        // Empty state
                        if self.groups.is_empty() {
                            ui.add_space(20.0);
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    egui::RichText::new("No groups created").size(12.0).weak(),
                                );
                                ui.label(
                                    egui::RichText::new("Click ＋ to create a group")
                                        .size(11.0)
                                        .weak(),
                                );
                            });
                        }
                    });
                }
            }
        });

        // Apply all pending actions
        for (id, enabled) in pending_enabled {
            self.set_light_enabled(&id, enabled);
        }
        for (id, update) in pending_light_updates {
            self.send_light_update(&id, update);
        }
        for (name, update) in pending_group_updates {
            self.send_group_update(&name, update);
        }
        if let Some(update) = pending_all_update {
            self.send_all_update(update);
        }
        for name in pending_delete_groups {
            self.delete_group(&name);
        }
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([320.0, 480.0])
            .with_title("Limekit")
            .with_resizable(true)
            .with_min_inner_size([280.0, 300.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Limekit Key Light",
        options,
        Box::new(|_cc| Ok(Box::new(KeylightApp::new()))),
    )
}
