use eframe::egui;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const DEFAULT_API_URL: &str = "http://127.0.0.1:9124";

mod colors {
    use eframe::egui::Color32;
    pub const BG_LIGHT: Color32 = Color32::from_rgb(245, 250, 255);
    pub const BG_CARD: Color32 = Color32::from_rgb(255, 255, 255);
    pub const ACCENT: Color32 = Color32::from_rgb(70, 150, 220);
    pub const ACCENT_LIGHT: Color32 = Color32::from_rgb(100, 175, 235);
    pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(30, 50, 80);
    pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(120, 140, 160);
    pub const BORDER: Color32 = Color32::from_rgb(200, 220, 240);
    pub const POWER_ON: Color32 = Color32::from_rgb(80, 190, 110);
    pub const POWER_OFF: Color32 = Color32::from_rgb(160, 170, 180);
    pub const WARM: Color32 = Color32::from_rgb(255, 170, 70);
    pub const COOL: Color32 = Color32::from_rgb(140, 195, 255);
    pub const BRIGHT_HIGH: Color32 = Color32::from_rgb(255, 252, 240);
    pub const BRIGHT_LOW: Color32 = Color32::from_rgb(50, 55, 65);
}

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

#[derive(Clone, Debug, Deserialize)]
struct LightStateResponse {
    id: String,
    on: bool,
    brightness: u8,
    kelvin: u16,
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
    on: bool,
    brightness: u8,
    kelvin: u16,
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Lights,
    Groups,
    Settings,
}

#[derive(PartialEq, Clone, Copy)]
enum ModalState {
    None,
    Discover,
    CreateGroup,
}

/// Pending update: (url, request)
type PendingUpdates = Arc<Mutex<HashMap<String, (String, UpdateRequest)>>>;

const AUTOSTART_DESKTOP: &str = r#"[Desktop Entry]
Type=Application
Name=SubLime
Comment=Elgato Key Light Controller
Exec=sublime
Icon=io.github.limebottle.SubLime
Terminal=false
Categories=Utility;
StartupNotify=false
"#;

fn get_autostart_path() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|p| p.join("autostart").join("sublime.desktop"))
}

fn is_autostart_enabled() -> bool {
    get_autostart_path().map(|p| p.exists()).unwrap_or(false)
}

fn set_autostart(enabled: bool) -> Result<(), std::io::Error> {
    let path = get_autostart_path()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "No config dir"))?;
    if enabled {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, AUTOSTART_DESKTOP)?;
    } else if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

struct KeylightApp {
    client: Arc<Client>,
    api_url: String,
    lights: Vec<LightControl>,
    groups: Vec<GroupRecord>,
    group_controls: HashMap<String, GroupControl>,
    active_tab: Tab,
    modal_state: ModalState,
    new_group_name: String,
    new_group_members: HashSet<String>,
    pending_updates: PendingUpdates,
    logo: Option<egui::TextureHandle>,
    power_icon: Option<egui::TextureHandle>,
    refresh_icon: Option<egui::TextureHandle>,
    all_on: bool,
    all_brightness: u8,
    all_kelvin: u16,
    editing_aliases: HashMap<String, String>,
    autostart_enabled: bool,
}

fn load_svg_texture(
    ctx: &egui::Context,
    name: &str,
    svg_data: &[u8],
    size: u32,
    white: bool,
) -> Option<egui::TextureHandle> {
    let mut svg_str = String::from_utf8_lossy(svg_data).to_string();
    if white {
        // Replace black fill with white
        svg_str = svg_str.replace("rgb(0,0,0)", "rgb(255,255,255)");
        svg_str = svg_str.replace("fill: rgb(0, 0, 0)", "fill: rgb(255, 255, 255)");
    }
    let opts = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(svg_str.as_bytes(), &opts).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size)?;
    let scale = size as f32 / tree.size().width().max(tree.size().height());
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let image =
        egui::ColorImage::from_rgba_unmultiplied([size as usize, size as usize], pixmap.data());
    Some(ctx.load_texture(name, image, egui::TextureOptions::LINEAR))
}

fn lerp_color(a: egui::Color32, b: egui::Color32, t: f32) -> egui::Color32 {
    egui::Color32::from_rgb(
        (a.r() as f32 + (b.r() as f32 - a.r() as f32) * t) as u8,
        (a.g() as f32 + (b.g() as f32 - a.g() as f32) * t) as u8,
        (a.b() as f32 + (b.b() as f32 - a.b() as f32) * t) as u8,
    )
}

/// Returns true if the value changed (queue updates on every change, deduplication happens in pending map)
fn brightness_slider(ui: &mut egui::Ui, value: &mut u8, width: f32) -> bool {
    let height = 18.0;
    let (rect, response) = ui.allocate_exact_size(
        egui::Vec2::new(width, height),
        egui::Sense::click_and_drag(),
    );

    let mut changed = false;
    if response.dragged() || response.clicked() {
        if let Some(pos) = ui.ctx().pointer_latest_pos() {
            let t = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            let new_val = (t * 100.0) as u8;
            if new_val != *value {
                *value = new_val;
                changed = true;
            }
        }
    }

    let rounding = height / 2.0;
    for i in 0..16 {
        let t = i as f32 / 16.0;
        let color = lerp_color(colors::BRIGHT_LOW, colors::BRIGHT_HIGH, t);
        let x = rect.left() + t * rect.width();
        let w = rect.width() / 16.0 + 1.0;
        let r = if i == 0 {
            egui::Rounding {
                nw: rounding,
                sw: rounding,
                ne: 0.0,
                se: 0.0,
            }
        } else if i == 15 {
            egui::Rounding {
                nw: 0.0,
                sw: 0.0,
                ne: rounding,
                se: rounding,
            }
        } else {
            egui::Rounding::ZERO
        };
        ui.painter().rect_filled(
            egui::Rect::from_min_size(egui::Pos2::new(x, rect.top()), egui::Vec2::new(w, height)),
            r,
            color,
        );
    }

    let thumb_x = (rect.left() + (*value as f32 / 100.0) * rect.width())
        .clamp(rect.left() + 8.0, rect.right() - 8.0);
    ui.painter().circle_filled(
        egui::Pos2::new(thumb_x, rect.center().y),
        8.0,
        egui::Color32::WHITE,
    );
    ui.painter().circle_stroke(
        egui::Pos2::new(thumb_x, rect.center().y),
        8.0,
        egui::Stroke::new(1.5, colors::ACCENT),
    );

    changed
}

/// Returns true if the value changed (queue updates on every change, deduplication happens in pending map)
fn temperature_slider(ui: &mut egui::Ui, kelvin: &mut u16, width: f32) -> bool {
    let height = 18.0;
    let (rect, response) = ui.allocate_exact_size(
        egui::Vec2::new(width, height),
        egui::Sense::click_and_drag(),
    );

    let mut changed = false;
    if response.dragged() || response.clicked() {
        if let Some(pos) = ui.ctx().pointer_latest_pos() {
            let t = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            let new_val = 2900 + (t * (7000.0 - 2900.0)) as u16;
            if new_val != *kelvin {
                *kelvin = new_val;
                changed = true;
            }
        }
    }

    let rounding = height / 2.0;
    for i in 0..16 {
        let t = i as f32 / 16.0;
        let color = lerp_color(colors::WARM, colors::COOL, t);
        let x = rect.left() + t * rect.width();
        let w = rect.width() / 16.0 + 1.0;
        let r = if i == 0 {
            egui::Rounding {
                nw: rounding,
                sw: rounding,
                ne: 0.0,
                se: 0.0,
            }
        } else if i == 15 {
            egui::Rounding {
                nw: 0.0,
                sw: 0.0,
                ne: rounding,
                se: rounding,
            }
        } else {
            egui::Rounding::ZERO
        };
        ui.painter().rect_filled(
            egui::Rect::from_min_size(egui::Pos2::new(x, rect.top()), egui::Vec2::new(w, height)),
            r,
            color,
        );
    }

    let t = (*kelvin as f32 - 2900.0) / (7000.0 - 2900.0);
    let thumb_x = (rect.left() + t * rect.width()).clamp(rect.left() + 8.0, rect.right() - 8.0);
    ui.painter().circle_filled(
        egui::Pos2::new(thumb_x, rect.center().y),
        8.0,
        egui::Color32::WHITE,
    );
    ui.painter().circle_stroke(
        egui::Pos2::new(thumb_x, rect.center().y),
        8.0,
        egui::Stroke::new(1.5, colors::ACCENT),
    );

    changed
}

fn power_button(
    ui: &mut egui::Ui,
    on: &mut bool,
    size: f32,
    icon: Option<&egui::TextureHandle>,
) -> bool {
    let (rect, response) = ui.allocate_exact_size(egui::Vec2::splat(size), egui::Sense::click());

    let bg = if *on {
        colors::POWER_ON
    } else {
        colors::POWER_OFF
    };
    ui.painter()
        .circle_filled(rect.center(), size / 2.0 - 1.0, bg);

    if let Some(tex) = icon {
        let icon_size = size * 0.65;
        let icon_rect = egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(icon_size));
        ui.painter().image(
            tex.id(),
            icon_rect,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    }

    if response.clicked() {
        *on = !*on;
        return true;
    }
    false
}

impl KeylightApp {
    fn new() -> Self {
        let api_url = std::env::var("KEYLIGHT_API_URL").unwrap_or_else(|_| DEFAULT_API_URL.into());
        let client = Arc::new(
            Client::builder()
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap(),
        );
        let pending_updates: PendingUpdates = Arc::new(Mutex::new(HashMap::new()));

        // Spawn worker thread that sends pending updates every 50ms
        {
            let client = Arc::clone(&client);
            let pending = Arc::clone(&pending_updates);
            thread::spawn(move || {
                loop {
                    thread::sleep(Duration::from_millis(50));
                    // Drain all pending updates and send them
                    let updates: Vec<(String, UpdateRequest)> = {
                        let mut map = pending.lock().unwrap();
                        map.drain().map(|(_, v)| v).collect()
                    };
                    for (url, req) in updates {
                        let _ = client.put(&url).json(&req).send();
                    }
                }
            });
        }

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
            pending_updates,
            logo: None,
            power_icon: None,
            refresh_icon: None,
            editing_aliases: HashMap::new(),
            all_on: true,
            all_brightness: 50,
            all_kelvin: 4500,
            autostart_enabled: is_autostart_enabled(),
        };
        app.refresh_all();
        app
    }

    fn ensure_textures(&mut self, ctx: &egui::Context) {
        if self.logo.is_none() {
            let bytes = include_bytes!("../../../../public/Limecon.png");
            if let Ok(img) = image::load_from_memory(bytes) {
                let rgba = img.to_rgba8();
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [rgba.width() as usize, rgba.height() as usize],
                    &rgba,
                );
                self.logo = Some(ctx.load_texture("logo", image, egui::TextureOptions::LINEAR));
            }
        }
        if self.power_icon.is_none() {
            let svg = include_bytes!("../../../../public/power.svg");
            self.power_icon = load_svg_texture(ctx, "power", svg, 64, true);
        }
        if self.refresh_icon.is_none() {
            let svg = include_bytes!("../../../../public/refresh.svg");
            self.refresh_icon = load_svg_texture(ctx, "refresh", svg, 64, true);
        }
    }

    fn refresh_all(&mut self) {
        self.refresh_lights();
        self.refresh_groups();
        self.refresh_light_states();
    }

    fn refresh_light_states(&mut self) {
        let url = format!("{}/v1/lights/states", self.api_url);
        if let Ok(res) = self
            .client
            .get(&url)
            .send()
            .and_then(|r| r.error_for_status())
        {
            if let Ok(states) = res.json::<Vec<LightStateResponse>>() {
                for state in states {
                    if let Some(light) = self.lights.iter_mut().find(|l| l.id == state.id) {
                        light.on = state.on;
                        light.brightness = state.brightness;
                        light.kelvin = state.kelvin;
                    }
                }
                self.sync_all_state();
            }
        }
    }

    fn sync_all_state(&mut self) {
        let enabled: Vec<_> = self.lights.iter().filter(|l| l.enabled).collect();
        if !enabled.is_empty() {
            self.all_on = enabled.iter().any(|l| l.on);
            self.all_brightness = (enabled.iter().map(|l| l.brightness as u32).sum::<u32>()
                / enabled.len() as u32) as u8;
            self.all_kelvin = (enabled.iter().map(|l| l.kelvin as u32).sum::<u32>()
                / enabled.len() as u32) as u16;
        }
    }

    fn refresh_lights(&mut self) {
        let url = format!("{}/v1/lights", self.api_url);
        if let Ok(res) = self
            .client
            .get(&url)
            .send()
            .and_then(|r| r.error_for_status())
        {
            if let Ok(records) = res.json::<Vec<LightRecord>>() {
                let mut updated = Vec::new();
                for record in records {
                    let label = record.alias.clone().unwrap_or_else(|| {
                        record
                            .name
                            .split('.')
                            .next()
                            .unwrap_or(&record.name)
                            .to_string()
                    });
                    let prev = self.lights.iter().find(|l| l.id == record.id).cloned();
                    updated.push(LightControl {
                        id: record.id.clone(),
                        label,
                        enabled: record.enabled,
                        on: prev.as_ref().map(|p| p.on).unwrap_or(true),
                        brightness: prev.as_ref().map(|p| p.brightness).unwrap_or(50),
                        kelvin: prev.as_ref().map(|p| p.kelvin).unwrap_or(4500),
                    });
                }
                self.lights = updated;
                for light in &self.lights {
                    self.editing_aliases
                        .entry(light.id.clone())
                        .or_insert_with(|| light.label.clone());
                }
            }
        }
    }

    fn refresh_groups(&mut self) {
        let url = format!("{}/v1/groups", self.api_url);
        if let Ok(res) = self
            .client
            .get(&url)
            .send()
            .and_then(|r| r.error_for_status())
        {
            if let Ok(groups) = res.json::<Vec<GroupRecord>>() {
                for g in &groups {
                    self.group_controls
                        .entry(g.name.clone())
                        .or_insert(GroupControl {
                            on: true,
                            brightness: 50,
                            kelvin: 4500,
                        });
                }
                self.groups = groups;
            }
        }
    }

    fn save_group(&mut self, name: String, members: Vec<String>) {
        let url = format!("{}/v1/groups", self.api_url);
        let _ = self
            .client
            .post(&url)
            .json(&GroupRequest { name, members })
            .send();
        self.refresh_groups();
    }

    fn delete_group(&mut self, name: &str) {
        let url = format!("{}/v1/groups/{}", self.api_url, urlencoding::encode(name));
        let _ = self.client.delete(&url).send();
        self.group_controls.remove(name);
        self.refresh_groups();
    }

    /// Queue an update - overwrites any pending update for the same key
    /// The worker thread sends these every 50ms, so only the latest value is sent
    fn queue_update(&self, key: &str, url: String, update: UpdateRequest) {
        let mut map = self.pending_updates.lock().unwrap();
        map.insert(key.to_string(), (url, update));
    }

    fn refresh_discovery(&mut self) {
        let url = format!("{}/v1/lights/refresh", self.api_url);
        let _ = self
            .client
            .post(&url)
            .json(&serde_json::json!({"timeout": 3}))
            .send();
        self.refresh_lights();
        self.refresh_light_states();
    }

    fn set_light_enabled(&mut self, id: &str, enabled: bool) {
        let url = format!(
            "{}/v1/lights/{}/enabled",
            self.api_url,
            urlencoding::encode(id)
        );
        let _ = self
            .client
            .put(&url)
            .json(&serde_json::json!({ "enabled": enabled }))
            .send();
    }

    fn set_light_alias(&mut self, id: &str, alias: &str) {
        let url = format!(
            "{}/v1/lights/{}/alias",
            self.api_url,
            urlencoding::encode(id)
        );
        let val: Option<&str> = if alias.trim().is_empty() {
            None
        } else {
            Some(alias.trim())
        };
        let _ = self
            .client
            .put(&url)
            .json(&serde_json::json!({ "alias": val }))
            .send();
        if let Some(l) = self.lights.iter_mut().find(|l| l.id == id) {
            l.label = if alias.trim().is_empty() {
                l.id.split('.').next().unwrap_or(&l.id).to_string()
            } else {
                alias.trim().to_string()
            };
        }
    }
}

impl eframe::App for KeylightApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ensure_textures(ctx);
        ctx.request_repaint(); // Keep UI responsive during drags

        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(4.0, 3.0);
        style.spacing.button_padding = egui::vec2(4.0, 2.0);
        ctx.set_style(style);

        let mut visuals = egui::Visuals::light();
        visuals.panel_fill = colors::BG_LIGHT;
        ctx.set_visuals(visuals);

        // Header
        egui::TopBottomPanel::top("header")
            .exact_height(40.0)
            .frame(
                egui::Frame::none()
                    .fill(colors::BG_CARD)
                    .inner_margin(egui::Margin::symmetric(8.0, 4.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if let Some(logo) = &self.logo {
                        ui.add(egui::Image::new(logo).fit_to_exact_size(egui::vec2(26.0, 26.0)));
                    }
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("SubLime")
                            .size(14.0)
                            .strong()
                            .color(colors::TEXT_PRIMARY),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (rect, response) =
                            ui.allocate_exact_size(egui::Vec2::splat(24.0), egui::Sense::click());
                        let bg = if response.hovered() {
                            colors::ACCENT_LIGHT
                        } else {
                            colors::ACCENT
                        };
                        ui.painter().rect_filled(rect, 4.0, bg);
                        if let Some(tex) = &self.refresh_icon {
                            let ir = egui::Rect::from_center_size(
                                rect.center(),
                                egui::Vec2::splat(17.0),
                            );
                            ui.painter().image(
                                tex.id(),
                                ir,
                                egui::Rect::from_min_max(
                                    egui::Pos2::ZERO,
                                    egui::Pos2::new(1.0, 1.0),
                                ),
                                egui::Color32::WHITE,
                            );
                        }
                        if response.clicked() {
                            self.refresh_all();
                        }
                    });
                });
            });

        // Tabs
        egui::TopBottomPanel::top("tabs")
            .exact_height(28.0)
            .frame(
                egui::Frame::none()
                    .fill(colors::BG_LIGHT)
                    .inner_margin(egui::Margin::symmetric(6.0, 2.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let lights_sel = self.active_tab == Tab::Lights;
                    let groups_sel = self.active_tab == Tab::Groups;
                    let settings_sel = self.active_tab == Tab::Settings;
                    if ui
                        .add(
                            egui::Button::new(egui::RichText::new("Lights").size(11.0).color(
                                if lights_sel {
                                    colors::ACCENT
                                } else {
                                    colors::TEXT_SECONDARY
                                },
                            ))
                            .fill(if lights_sel {
                                colors::BG_CARD
                            } else {
                                egui::Color32::TRANSPARENT
                            })
                            .rounding(3.0)
                            .min_size(egui::vec2(50.0, 20.0)),
                        )
                        .clicked()
                    {
                        self.active_tab = Tab::Lights;
                        self.modal_state = ModalState::None;
                    }
                    if ui
                        .add(
                            egui::Button::new(egui::RichText::new("Groups").size(11.0).color(
                                if groups_sel {
                                    colors::ACCENT
                                } else {
                                    colors::TEXT_SECONDARY
                                },
                            ))
                            .fill(if groups_sel {
                                colors::BG_CARD
                            } else {
                                egui::Color32::TRANSPARENT
                            })
                            .rounding(3.0)
                            .min_size(egui::vec2(50.0, 20.0)),
                        )
                        .clicked()
                    {
                        self.active_tab = Tab::Groups;
                        self.modal_state = ModalState::None;
                    }
                    if ui
                        .add(
                            egui::Button::new(egui::RichText::new("Settings").size(11.0).color(
                                if settings_sel {
                                    colors::ACCENT
                                } else {
                                    colors::TEXT_SECONDARY
                                },
                            ))
                            .fill(if settings_sel {
                                colors::BG_CARD
                            } else {
                                egui::Color32::TRANSPARENT
                            })
                            .rounding(3.0)
                            .min_size(egui::vec2(50.0, 20.0)),
                        )
                        .clicked()
                    {
                        self.active_tab = Tab::Settings;
                        self.modal_state = ModalState::None;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Only show + button for Lights and Groups tabs
                        if self.active_tab != Tab::Settings {
                            let (rect, response) = ui
                                .allocate_exact_size(egui::Vec2::splat(22.0), egui::Sense::click());
                            let bg = if response.hovered() {
                                colors::ACCENT_LIGHT
                            } else {
                                colors::ACCENT
                            };
                            ui.painter().rect_filled(rect, 4.0, bg);
                            let c = rect.center();
                            let arm = 5.0;
                            let s = egui::Stroke::new(2.0, egui::Color32::WHITE);
                            ui.painter().line_segment(
                                [
                                    egui::Pos2::new(c.x - arm, c.y),
                                    egui::Pos2::new(c.x + arm, c.y),
                                ],
                                s,
                            );
                            ui.painter().line_segment(
                                [
                                    egui::Pos2::new(c.x, c.y - arm),
                                    egui::Pos2::new(c.x, c.y + arm),
                                ],
                                s,
                            );
                            if response.clicked() {
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
                                    Tab::Settings => ModalState::None,
                                };
                            }
                        }
                    });
                });
            });

        // Main
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(colors::BG_LIGHT)
                    .inner_margin(egui::Margin::same(6.0)),
            )
            .show(ctx, |ui| {
                let w = ui.available_width();
                let power_tex = self.power_icon.clone();

                match self.active_tab {
                    Tab::Lights => {
                        if self.modal_state == ModalState::Discover {
                            egui::Frame::none()
                                .fill(colors::BG_CARD)
                                .stroke(egui::Stroke::new(1.0, colors::BORDER))
                                .rounding(6.0)
                                .inner_margin(8.0)
                                .show(ui, |ui| {
                                    ui.set_width(w - 4.0);
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new("Manage Lights")
                                                .size(12.0)
                                                .strong()
                                                .color(colors::TEXT_PRIMARY),
                                        );
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if ui.small_button("Done").clicked() {
                                                    self.modal_state = ModalState::None;
                                                }
                                            },
                                        );
                                    });
                                    if ui.small_button("Scan").clicked() {
                                        self.refresh_discovery();
                                    }
                                    let mut pending: Vec<(String, bool)> = Vec::new();
                                    let mut pending_aliases: Vec<(String, String)> = Vec::new();
                                    for idx in 0..self.lights.len() {
                                        let id = self.lights[idx].id.clone();
                                        let mut en = self.lights[idx].enabled;
                                        let alias = self
                                            .editing_aliases
                                            .entry(id.clone())
                                            .or_insert_with(|| self.lights[idx].label.clone());
                                        ui.horizontal(|ui| {
                                            if ui.checkbox(&mut en, "").changed() {
                                                self.lights[idx].enabled = en;
                                                pending.push((id.clone(), en));
                                            }
                                            let r = ui.add(
                                                egui::TextEdit::singleline(alias)
                                                    .desired_width(w - 40.0),
                                            );
                                            if r.lost_focus() {
                                                pending_aliases.push((id.clone(), alias.clone()));
                                            }
                                        });
                                    }
                                    for (id, en) in pending {
                                        self.set_light_enabled(&id, en);
                                    }
                                    for (id, al) in pending_aliases {
                                        self.set_light_alias(&id, &al);
                                    }
                                });
                            ui.add_space(4.0);
                        }

                        // All lights
                        egui::Frame::none()
                            .fill(colors::BG_CARD)
                            .stroke(egui::Stroke::new(1.0, colors::BORDER))
                            .rounding(6.0)
                            .inner_margin(8.0)
                            .show(ui, |ui| {
                                ui.set_width(w - 4.0);
                                ui.horizontal(|ui| {
                                    if power_button(ui, &mut self.all_on, 26.0, power_tex.as_ref())
                                    {
                                        let state = self.all_on;
                                        for l in &mut self.lights {
                                            if l.enabled {
                                                l.on = state;
                                            }
                                        }
                                        let url = format!("{}/v1/all", self.api_url);
                                        self.queue_update(
                                            "all_power",
                                            url,
                                            UpdateRequest {
                                                on: Some(if state { 1 } else { 0 }),
                                                brightness: None,
                                                kelvin: None,
                                                mired: None,
                                            },
                                        );
                                    }
                                    ui.add_space(4.0);
                                    ui.label(
                                        egui::RichText::new("All Lights")
                                            .size(11.0)
                                            .strong()
                                            .color(colors::TEXT_PRIMARY),
                                    );
                                });
                                ui.add_space(2.0);
                                let sw = w - 16.0;
                                let mut b = self.all_brightness;
                                let mut k = self.all_kelvin;
                                if brightness_slider(ui, &mut b, sw) {
                                    self.all_brightness = b;
                                    for l in &mut self.lights {
                                        if l.enabled {
                                            l.brightness = b;
                                        }
                                    }
                                    let url = format!("{}/v1/all", self.api_url);
                                    self.queue_update(
                                        "all_b",
                                        url,
                                        UpdateRequest {
                                            on: None,
                                            brightness: Some(b),
                                            kelvin: None,
                                            mired: None,
                                        },
                                    );
                                }
                                ui.add_space(1.0);
                                if temperature_slider(ui, &mut k, sw) {
                                    self.all_kelvin = k;
                                    for l in &mut self.lights {
                                        if l.enabled {
                                            l.kelvin = k;
                                        }
                                    }
                                    let url = format!("{}/v1/all", self.api_url);
                                    self.queue_update(
                                        "all_k",
                                        url,
                                        UpdateRequest {
                                            on: None,
                                            brightness: None,
                                            kelvin: Some(k),
                                            mired: None,
                                        },
                                    );
                                }
                            });
                        ui.add_space(3.0);

                        // Individual lights
                        for index in 0..self.lights.len() {
                            if !self.lights[index].enabled {
                                continue;
                            }
                            let id = self.lights[index].id.clone();
                            let label = self.lights[index].label.clone();
                            let mut on = self.lights[index].on;
                            let mut b = self.lights[index].brightness;
                            let mut k = self.lights[index].kelvin;
                            let pt = power_tex.clone();

                            egui::Frame::none()
                                .fill(colors::BG_CARD)
                                .stroke(egui::Stroke::new(1.0, colors::BORDER))
                                .rounding(6.0)
                                .inner_margin(8.0)
                                .show(ui, |ui| {
                                    ui.set_width(w - 4.0);
                                    ui.horizontal(|ui| {
                                        if power_button(ui, &mut on, 26.0, pt.as_ref()) {
                                            self.lights[index].on = on;
                                            let url = format!(
                                                "{}/v1/lights/{}",
                                                self.api_url,
                                                urlencoding::encode(&id)
                                            );
                                            self.queue_update(
                                                &format!("p_{}", id),
                                                url,
                                                UpdateRequest {
                                                    on: Some(if on { 1 } else { 0 }),
                                                    brightness: None,
                                                    kelvin: None,
                                                    mired: None,
                                                },
                                            );
                                            self.sync_all_state();
                                        }
                                        ui.add_space(4.0);
                                        ui.label(
                                            egui::RichText::new(&label)
                                                .size(11.0)
                                                .strong()
                                                .color(colors::TEXT_PRIMARY),
                                        );
                                    });
                                    ui.add_space(2.0);
                                    let sw = w - 16.0;
                                    if brightness_slider(ui, &mut b, sw) {
                                        self.lights[index].brightness = b;
                                        let url = format!(
                                            "{}/v1/lights/{}",
                                            self.api_url,
                                            urlencoding::encode(&id)
                                        );
                                        self.queue_update(
                                            &format!("b_{}", id),
                                            url,
                                            UpdateRequest {
                                                on: None,
                                                brightness: Some(b),
                                                kelvin: None,
                                                mired: None,
                                            },
                                        );
                                    }
                                    ui.add_space(1.0);
                                    if temperature_slider(ui, &mut k, sw) {
                                        self.lights[index].kelvin = k;
                                        let url = format!(
                                            "{}/v1/lights/{}",
                                            self.api_url,
                                            urlencoding::encode(&id)
                                        );
                                        self.queue_update(
                                            &format!("k_{}", id),
                                            url,
                                            UpdateRequest {
                                                on: None,
                                                brightness: None,
                                                kelvin: Some(k),
                                                mired: None,
                                            },
                                        );
                                    }
                                });
                            ui.add_space(3.0);
                        }

                        if self.lights.iter().filter(|l| l.enabled).count() == 0
                            && self.modal_state == ModalState::None
                        {
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    egui::RichText::new("No lights. Click + to discover.")
                                        .size(10.0)
                                        .color(colors::TEXT_SECONDARY),
                                );
                            });
                        }
                    }

                    Tab::Groups => {
                        if self.modal_state == ModalState::CreateGroup {
                            egui::Frame::none()
                                .fill(colors::BG_CARD)
                                .stroke(egui::Stroke::new(1.0, colors::BORDER))
                                .rounding(6.0)
                                .inner_margin(8.0)
                                .show(ui, |ui| {
                                    ui.set_width(w - 4.0);
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new("Create Group")
                                                .size(12.0)
                                                .strong()
                                                .color(colors::TEXT_PRIMARY),
                                        );
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if ui.small_button("×").clicked() {
                                                    self.modal_state = ModalState::None;
                                                }
                                            },
                                        );
                                    });
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.new_group_name)
                                            .hint_text("Name")
                                            .desired_width(w - 16.0),
                                    );
                                    for light in &self.lights {
                                        let mut sel = self.new_group_members.contains(&light.id);
                                        if ui.checkbox(&mut sel, &light.label).changed() {
                                            if sel {
                                                self.new_group_members.insert(light.id.clone());
                                            } else {
                                                self.new_group_members.remove(&light.id);
                                            }
                                        }
                                    }
                                    let can = !self.new_group_name.trim().is_empty()
                                        && !self.new_group_members.is_empty();
                                    ui.add_enabled_ui(can, |ui| {
                                        if ui.small_button("Save").clicked() {
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
                            ui.add_space(4.0);
                        }

                        let groups = self.groups.clone();
                        for group in groups {
                            let ctrl = self.group_controls.entry(group.name.clone()).or_insert(
                                GroupControl {
                                    on: true,
                                    brightness: 50,
                                    kelvin: 4500,
                                },
                            );
                            let mut on = ctrl.on;
                            let mut b = ctrl.brightness;
                            let mut k = ctrl.kelvin;
                            let name = group.name.clone();
                            let pt = power_tex.clone();

                            egui::Frame::none()
                                .fill(colors::BG_CARD)
                                .stroke(egui::Stroke::new(1.0, colors::BORDER))
                                .rounding(6.0)
                                .inner_margin(8.0)
                                .show(ui, |ui| {
                                    ui.set_width(w - 4.0);
                                    ui.horizontal(|ui| {
                                        if power_button(ui, &mut on, 26.0, pt.as_ref()) {
                                            if let Some(c) = self.group_controls.get_mut(&name) {
                                                c.on = on;
                                            }
                                            let url = format!(
                                                "{}/v1/groups/{}",
                                                self.api_url,
                                                urlencoding::encode(&name)
                                            );
                                            self.queue_update(
                                                &format!("gp_{}", name),
                                                url,
                                                UpdateRequest {
                                                    on: Some(if on { 1 } else { 0 }),
                                                    brightness: None,
                                                    kelvin: None,
                                                    mired: None,
                                                },
                                            );
                                        }
                                        ui.add_space(4.0);
                                        ui.label(
                                            egui::RichText::new(&name)
                                                .size(11.0)
                                                .strong()
                                                .color(colors::TEXT_PRIMARY),
                                        );
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "({})",
                                                group.members.len()
                                            ))
                                            .size(9.0)
                                            .color(colors::TEXT_SECONDARY),
                                        );
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if ui.small_button("×").clicked() {
                                                    self.delete_group(&name);
                                                }
                                            },
                                        );
                                    });
                                    ui.add_space(2.0);
                                    let sw = w - 16.0;
                                    if brightness_slider(ui, &mut b, sw) {
                                        if let Some(c) = self.group_controls.get_mut(&name) {
                                            c.brightness = b;
                                        }
                                        let url = format!(
                                            "{}/v1/groups/{}",
                                            self.api_url,
                                            urlencoding::encode(&name)
                                        );
                                        self.queue_update(
                                            &format!("gb_{}", name),
                                            url,
                                            UpdateRequest {
                                                on: None,
                                                brightness: Some(b),
                                                kelvin: None,
                                                mired: None,
                                            },
                                        );
                                    }
                                    ui.add_space(1.0);
                                    if temperature_slider(ui, &mut k, sw) {
                                        if let Some(c) = self.group_controls.get_mut(&name) {
                                            c.kelvin = k;
                                        }
                                        let url = format!(
                                            "{}/v1/groups/{}",
                                            self.api_url,
                                            urlencoding::encode(&name)
                                        );
                                        self.queue_update(
                                            &format!("gk_{}", name),
                                            url,
                                            UpdateRequest {
                                                on: None,
                                                brightness: None,
                                                kelvin: Some(k),
                                                mired: None,
                                            },
                                        );
                                    }
                                });
                            ui.add_space(3.0);
                        }

                        if self.groups.is_empty() && self.modal_state == ModalState::None {
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    egui::RichText::new("No groups. Click + to create.")
                                        .size(10.0)
                                        .color(colors::TEXT_SECONDARY),
                                );
                            });
                        }
                    }

                    Tab::Settings => {
                        egui::Frame::none()
                            .fill(colors::BG_CARD)
                            .stroke(egui::Stroke::new(1.0, colors::BORDER))
                            .rounding(6.0)
                            .inner_margin(12.0)
                            .show(ui, |ui| {
                                ui.set_width(w - 4.0);
                                ui.label(
                                    egui::RichText::new("Settings")
                                        .size(13.0)
                                        .strong()
                                        .color(colors::TEXT_PRIMARY),
                                );
                                ui.add_space(8.0);

                                // Autostart toggle
                                ui.horizontal(|ui| {
                                    let mut autostart = self.autostart_enabled;
                                    if ui.checkbox(&mut autostart, "").changed()
                                        && set_autostart(autostart).is_ok()
                                    {
                                        self.autostart_enabled = autostart;
                                    }
                                    ui.label(
                                        egui::RichText::new("Start on login")
                                            .size(11.0)
                                            .color(colors::TEXT_PRIMARY),
                                    );
                                });
                                ui.label(
                                    egui::RichText::new(
                                        "Launch SubLime automatically when you log in",
                                    )
                                    .size(9.0)
                                    .color(colors::TEXT_SECONDARY),
                                );

                                ui.add_space(12.0);
                                ui.separator();
                                ui.add_space(8.0);

                                // About section
                                ui.label(
                                    egui::RichText::new("About")
                                        .size(11.0)
                                        .strong()
                                        .color(colors::TEXT_PRIMARY),
                                );
                                ui.add_space(4.0);
                                ui.label(
                                    egui::RichText::new("SubLime v0.1.0")
                                        .size(10.0)
                                        .color(colors::TEXT_SECONDARY),
                                );
                                ui.label(
                                    egui::RichText::new("Elgato Key Light Controller for Linux")
                                        .size(10.0)
                                        .color(colors::TEXT_SECONDARY),
                                );
                            });
                    }
                }
            });
    }
}

/// Check if the daemon is already running by pinging the health endpoint
fn daemon_is_running(api_url: &str) -> bool {
    let client = Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
        .ok();
    if let Some(c) = client {
        c.get(format!("{}/v1/lights", api_url)).send().is_ok()
    } else {
        false
    }
}

/// Spawn the keylightd daemon process
fn spawn_daemon() -> Option<std::process::Child> {
    // Try to find keylightd in same directory as this executable, or in PATH
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let daemon_path = exe_dir.join("keylightd");

    let path = if daemon_path.exists() {
        daemon_path
    } else {
        // Fall back to PATH
        std::path::PathBuf::from("keylightd")
    };

    std::process::Command::new(path)
        .arg("serve")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()
}

fn main() -> eframe::Result<()> {
    let api_url = std::env::var("KEYLIGHT_API_URL").unwrap_or_else(|_| DEFAULT_API_URL.into());

    // Start daemon if not already running
    let mut daemon_process: Option<std::process::Child> = None;
    if !daemon_is_running(&api_url) {
        eprintln!("Starting keylightd daemon...");
        daemon_process = spawn_daemon();
        // Give daemon time to start
        thread::sleep(Duration::from_millis(500));
    }

    // Set the window/taskbar icon to Limecon.png.
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../../../../public/Limecon.png"))
        .unwrap_or_default();

    let result = eframe::run_native(
        "SubLime",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([300.0, 360.0])
                // Important on KDE/Wayland: Plasma uses this app-id to look up the icon from the .desktop file.
                .with_app_id("io.github.limebottle.SubLime")
                .with_title("SubLime")
                .with_resizable(true)
                .with_min_inner_size([280.0, 280.0])
                .with_close_button(true)
                .with_icon(icon),
            ..Default::default()
        },
        Box::new(|_cc| Ok(Box::new(KeylightApp::new()))),
    );

    // Clean up daemon when app exits
    if let Some(mut child) = daemon_process {
        let _ = child.kill();
    }

    result
}
