//! Fenêtre de réglages façon NZXT CAM : rendu CPU (egui + egui_software_backend), taille fixe
//! 960×640, mise en page absolue pour des cartes alignées au pixel.

use crate::config::{Config, LcdMode, Mode, Profile, Reading};
use crate::daemon;
use crate::kraken::{self, CRITICAL_TEMP, CURVE_MIN_TEMP, Channel, Kraken, Status};
use crate::render::{self, Fonts, Values};
use crate::sensors::{Readings, Sensors};
use crate::system;
use egui::epaint::{ColorMode, Mesh, PathShape, PathStroke};
use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align2, Color32, ColorImage, CursorIcon, FontFamily, FontId, Painter, Pos2, Rect, Sense, Shape, Stroke, StrokeKind,
    TextureHandle, TextureOptions, Ui, Vec2, pos2, vec2,
};
use egui_software_backend::{SoftwareBackend, SoftwareBackendAppConfiguration};
use std::collections::VecDeque;
use std::hash::Hash;
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
use windows_sys::Win32::System::Threading::CreateMutexW;

const POLL: Duration = Duration::from_secs(1);
const RECONNECT: Duration = Duration::from_secs(3);
/// Échantillons gardés pour les mini-graphes (1 par seconde).
const HISTORY: usize = 120;

const WIDTH: f32 = 960.0;
const HEIGHT: f32 = 640.0;
const SIDEBAR_W: f32 = 216.0;
const PAD: f32 = 28.0;
const GAP: f32 = 16.0;
const TOP: f32 = 92.0;
/// Rayon de l'aperçu de l'écran : tableau de bord, page Écran.
const DASH_DISC: f32 = 64.0;
const LCD_DISC: f32 = 128.0;

// Palette inspirée de NZXT CAM.
const BG: Color32 = Color32::from_rgb(11, 11, 15);
const SIDEBAR: Color32 = Color32::from_rgb(17, 17, 22);
const CARD: Color32 = Color32::from_rgb(23, 23, 30);
const CARD_HI: Color32 = Color32::from_rgb(33, 33, 43);
const FIELD: Color32 = Color32::from_rgb(15, 15, 20);
const TRACK: Color32 = Color32::from_rgb(40, 40, 52);
const BORDER: Color32 = Color32::from_rgb(36, 36, 47);
const TEXT: Color32 = Color32::from_rgb(238, 238, 244);
const MUTED: Color32 = Color32::from_rgb(136, 138, 155);
const FAINT: Color32 = Color32::from_rgb(88, 90, 105);
const ACCENT: Color32 = Color32::from_rgb(124, 58, 237);
const ACCENT_HI: Color32 = Color32::from_rgb(167, 128, 250);
const PINK: Color32 = Color32::from_rgb(217, 70, 239);
const CYAN: Color32 = Color32::from_rgb(34, 211, 238);
const BLUE: Color32 = Color32::from_rgb(59, 130, 246);
const OK: Color32 = Color32::from_rgb(52, 211, 153);
const WARN: Color32 = Color32::from_rgb(251, 191, 36);
const ERR: Color32 = Color32::from_rgb(248, 113, 113);

const GUI_MUTEX: &str = r"Local\nzxt-lite-gui";

pub fn run() -> Result<(), String> {
    // Une seule fenêtre : une seconde instance ramène la première au premier plan.
    let name: Vec<u16> = GUI_MUTEX.encode_utf16().chain(Some(0)).collect();
    let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if !mutex.is_null() && unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        system::focus_gui();
        unsafe { CloseHandle(mutex) };
        return Ok(());
    }
    let viewport = egui::ViewportBuilder::default()
        .with_title(system::GUI_TITLE)
        .with_inner_size(vec2(WIDTH, HEIGHT))
        .with_resizable(false)
        .with_maximize_button(false);
    let settings = SoftwareBackendAppConfiguration::new().viewport_builder(viewport);
    let result = egui_software_backend::run_app_with_software_backend(settings, App::new).map_err(|e| format!("{e:?}"));
    if !mutex.is_null() {
        unsafe { CloseHandle(mutex) };
    }
    result
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Dashboard,
    Cooling,
    Lcd,
    Settings,
}

/// Historique d'une mesure et extrêmes de la session.
#[derive(Default)]
struct Track {
    samples: VecDeque<f32>,
    range: Option<(f32, f32)>,
}

impl Track {
    fn push(&mut self, v: Option<f32>) {
        let Some(v) = v else { return };
        if self.samples.len() == HISTORY {
            self.samples.pop_front();
        }
        self.samples.push_back(v);
        self.range = Some(self.range.map_or((v, v), |(lo, hi)| (lo.min(v), hi.max(v))));
    }
}

#[derive(Default)]
struct History {
    cpu: Track,
    gpu: Track,
    liquid: Track,
    pump: Track,
    fan: Track,
}

struct App {
    page: Page,
    channel: Channel,
    kraken: Option<Kraken>,
    firmware: String,
    status: Option<Status>,
    sensors: Sensors,
    readings: Readings,
    history: History,
    last_poll: Instant,
    last_connect: Instant,
    cfg: Config,
    saved: Config,
    /// Dernière config envoyée directement, sans le service (pour savoir s'il faut renvoyer l'image).
    applied: Option<Config>,
    autostart: bool,
    cam_autostart: bool,
    cam_running: bool,
    daemon_running: bool,
    image_text: String,
    fonts: Fonts,
    preview: Option<TextureHandle>,
    preview_key: Option<(Values, String, u32)>,
    /// Image/GIF choisi pour l'écran (première image), par chemin et taille.
    picture: Option<(String, u32, Option<TextureHandle>)>,
    message: Option<(String, bool)>,
    icon_set: bool,
}

// ---------- typographie & style ----------

fn regular(size: f32) -> FontId {
    FontId::proportional(size)
}

fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semibold".into()))
}

fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}

/// Segoe UI (normal, semi-gras, gras), partagée avec le rendu de l'écran ; police embarquée d'egui à défaut.
fn setup_fonts(ctx: &egui::Context) {
    let mut defs = egui::FontDefinitions::empty();
    for (name, file) in [("regular", "segoeui.ttf"), ("semibold", "seguisb.ttf"), ("bold", "segoeuib.ttf")] {
        if let Some(data) = render::system_font(file) {
            defs.font_data.insert(name.into(), Arc::new(egui::FontData::from_static(data)));
        }
    }
    let family = |name: &str| FontFamily::Name(name.into());
    if defs.font_data.len() == 3 {
        for (f, font) in [
            (FontFamily::Proportional, "regular"),
            (FontFamily::Monospace, "regular"),
            (family("semibold"), "semibold"),
            (family("bold"), "bold"),
        ] {
            defs.families.insert(f, vec![font.into()]);
        }
    } else {
        defs = egui::FontDefinitions::default();
        let base = defs.families[&FontFamily::Proportional].clone();
        defs.families.insert(family("semibold"), base.clone());
        defs.families.insert(family("bold"), base);
    }
    ctx.set_fonts(defs);
}

fn style(ctx: &egui::Context) {
    setup_fonts(ctx);
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = CARD;
    v.extreme_bg_color = FIELD;
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = ACCENT;
    v.selection.stroke = Stroke::new(1.0_f32, TEXT);
    v.text_cursor.stroke = Stroke::new(2.0_f32, ACCENT_HI);
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.noninteractive] {
        w.corner_radius = 8.into();
        w.bg_stroke = Stroke::new(1.0_f32, BORDER);
    }
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, ACCENT_HI);
    v.widgets.active.bg_stroke = Stroke::new(1.0_f32, ACCENT);
    ctx.set_visuals(v);
}

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

fn fmt_temp(v: Option<f32>) -> String {
    v.map_or("--".into(), |t| format!("{t:.0}°"))
}

/// Image ronde pour l'aperçu : réduite à la taille d'affichage, transparente hors du disque
/// (bord anti-crénelé) — pas de masque à peindre par-dessus.
fn round_image(img: &image::RgbaImage, size: u32) -> ColorImage {
    let mut img = image::imageops::resize(img, size, size, image::imageops::FilterType::CatmullRom);
    let c = size as f32 / 2.0;
    for (x, y, px) in img.enumerate_pixels_mut() {
        let d = ((x as f32 + 0.5 - c).powi(2) + (y as f32 + 0.5 - c).powi(2)).sqrt();
        px.0[3] = (px.0[3] as f32 * (c - d + 0.5).clamp(0.0, 1.0)) as u8;
    }
    ColorImage::from_rgba_unmultiplied([size as usize; 2], img.as_raw())
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect::from_min_size(pos2(x, y), vec2(w, h))
}

fn text_width(ui: &Ui, text: &str, font: FontId) -> f32 {
    ui.painter().layout_no_wrap(text.to_owned(), font, TEXT).size().x
}

// ---------- composants ----------

fn card(p: &Painter, r: Rect) {
    p.rect(r, 14, CARD, Stroke::new(1.0_f32, BORDER), StrokeKind::Inside);
}

/// Petit titre en capitales espacées.
fn caption(p: &Painter, pos: Pos2, text: &str) {
    let mut job = LayoutJob::default();
    let format = TextFormat { font_id: semibold(11.5), color: MUTED, extra_letter_spacing: 1.2, ..Default::default() };
    job.append(&text.to_uppercase(), 0.0, format);
    let galley = p.layout_job(job);
    p.galley(pos, galley, MUTED);
}

/// Étiquette arrondie alignée à droite sur `right`, centrée verticalement sur `cy`.
fn chip(ui: &Ui, right: f32, cy: f32, text: &str, fg: Color32) -> Rect {
    let w = text_width(ui, text, semibold(11.5)) + 20.0;
    let r = rect(right - w, cy - 11.0, w, 22.0);
    let p = ui.painter();
    p.rect_filled(r, 11, fg.gamma_multiply(0.16));
    p.text(r.center(), Align2::CENTER_CENTER, text, semibold(11.5), fg);
    r
}

fn dot(p: &Painter, c: Pos2, color: Color32) {
    p.circle_filled(c, 4.0, color);
}

fn hand_cursor(ui: &Ui, resp: &egui::Response) {
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
}

/// Préréglage en forme de pilule ; renvoie `true` au clic.
fn pill(ui: &mut Ui, r: Rect, id: impl Hash, text: &str, selected: bool) -> bool {
    let resp = ui.interact(r, ui.id().with(id), Sense::click());
    hand_cursor(ui, &resp);
    let (fill, stroke, color) = match (selected, resp.hovered()) {
        (true, _) => (ACCENT, ACCENT, Color32::WHITE),
        (false, true) => (CARD_HI, ACCENT_HI.gamma_multiply(0.6), TEXT),
        (false, false) => (CARD_HI, BORDER, MUTED),
    };
    let p = ui.painter();
    p.rect(r, r.height() / 2.0, fill, Stroke::new(1.0_f32, stroke), StrokeKind::Inside);
    p.text(r.center(), Align2::CENTER_CENTER, text, semibold(13.0), color);
    resp.clicked()
}

/// Rangée de pilules à largeur ajustée au texte ; renvoie l'index cliqué.
fn pills(ui: &mut Ui, origin: Pos2, id: impl Hash + Copy, items: &[(&str, bool)]) -> Option<usize> {
    let mut x = origin.x;
    let mut clicked = None;
    for (i, (text, selected)) in items.iter().enumerate() {
        let w = text_width(ui, text, semibold(13.0)) + 34.0;
        if pill(ui, rect(x, origin.y, w, 32.0), (id, i), text, *selected) {
            clicked = Some(i);
        }
        x += w + 8.0;
    }
    clicked
}

/// Contrôle segmenté à parts égales ; renvoie l'index cliqué.
fn segmented(ui: &mut Ui, r: Rect, id: impl Hash + Copy, items: &[&str], selected: Option<usize>) -> Option<usize> {
    ui.painter().rect(r, 10, FIELD, Stroke::new(1.0_f32, BORDER), StrokeKind::Inside);
    let w = (r.width() - 6.0) / items.len() as f32;
    let mut clicked = None;
    for (i, text) in items.iter().enumerate() {
        let seg = rect(r.left() + 3.0 + w * i as f32, r.top() + 3.0, w, r.height() - 6.0);
        let resp = ui.interact(seg, ui.id().with((id, i)), Sense::click());
        hand_cursor(ui, &resp);
        let on = selected == Some(i);
        let p = ui.painter();
        if on {
            p.rect_filled(seg, 8, ACCENT);
        } else if resp.hovered() {
            p.rect_filled(seg, 8, CARD_HI);
        }
        let color = if on { Color32::WHITE } else if resp.hovered() { TEXT } else { MUTED };
        p.text(seg.center(), Align2::CENTER_CENTER, *text, semibold(12.5), color);
        if resp.clicked() {
            clicked = Some(i);
        }
    }
    clicked
}

/// Interrupteur 44×24 ; renvoie `true` s'il a basculé.
fn toggle(ui: &mut Ui, r: Rect, id: impl Hash, on: &mut bool) -> bool {
    let id = ui.id().with(id);
    let resp = ui.interact(r, id, Sense::click());
    hand_cursor(ui, &resp);
    if resp.clicked() {
        *on = !*on;
    }
    let t = ui.ctx().animate_bool_with_time(id, *on, 0.12);
    let p = ui.painter();
    p.rect(r, r.height() / 2.0, lerp(TRACK, ACCENT, t), Stroke::NONE, StrokeKind::Inside);
    let x = r.left() + 12.0 + t * (r.width() - 24.0);
    p.circle_filled(pos2(x, r.center().y), 8.5, Color32::WHITE);
    resp.clicked()
}

/// Curseur horizontal sur `min..=max` ; renvoie `true` si la valeur a changé.
fn slider(ui: &mut Ui, r: Rect, id: impl Hash, value: &mut u8, min: u8, max: u8) -> bool {
    let resp = ui.interact(r, ui.id().with(id), Sense::click_and_drag());
    hand_cursor(ui, &resp);
    let track = Rect::from_center_size(r.center(), vec2(r.width() - 20.0, 6.0));
    let old = *value;
    if let Some(pos) = resp.interact_pointer_pos().filter(|_| resp.dragged() || resp.clicked()) {
        let f = ((pos.x - track.left()) / track.width()).clamp(0.0, 1.0);
        *value = (min as f32 + f * (max - min) as f32).round() as u8;
    }
    let f = (*value - min) as f32 / (max - min) as f32;
    let x = track.left() + f * track.width();
    let p = ui.painter();
    p.rect_filled(track, 3, TRACK);
    p.rect_filled(Rect::from_min_max(track.min, pos2(x, track.max.y)), 3, ACCENT);
    let hot = resp.hovered() || resp.dragged();
    p.circle(pos2(x, track.center().y), if hot { 10.0 } else { 9.0 }, Color32::WHITE, Stroke::new(3.0_f32, ACCENT));
    *value != old
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Primary,
    Ghost,
}

fn button(ui: &mut Ui, r: Rect, id: impl Hash, text: &str, kind: Kind, enabled: bool) -> bool {
    let resp = ui.interact(r, ui.id().with(id), if enabled { Sense::click() } else { Sense::hover() });
    if enabled {
        hand_cursor(ui, &resp);
    }
    let hot = enabled && resp.hovered();
    let (fill, stroke, color) = match (kind, enabled) {
        (Kind::Primary, true) => (if hot { lerp(ACCENT, ACCENT_HI, 0.25) } else { ACCENT }, Stroke::NONE, Color32::WHITE),
        (Kind::Ghost, true) => (if hot { CARD_HI } else { CARD }, Stroke::new(1.0_f32, if hot { ACCENT_HI } else { BORDER }), TEXT),
        (_, false) => (CARD, Stroke::new(1.0_f32, BORDER), FAINT),
    };
    let p = ui.painter();
    p.rect(r, 9, fill, stroke, StrokeKind::Inside);
    p.text(r.center(), Align2::CENTER_CENTER, text, semibold(13.5), color);
    enabled && resp.clicked()
}

fn arc(c: Pos2, r: f32, from: f32, to: f32) -> Vec<Pos2> {
    let n = ((to - from).abs() / 3.0).ceil().max(2.0) as usize;
    (0..=n)
        .map(|i| {
            let a = (from + (to - from) * i as f32 / n as f32).to_radians();
            pos2(c.x + r * a.cos(), c.y + r * a.sin())
        })
        .collect()
}

/// Jauge 270° à dégradé, bouts arrondis ; `frac` dans 0..=1.
fn gauge(p: &Painter, c: Pos2, r: f32, width: f32, frac: Option<f32>, from: Color32, to: Color32) {
    const START: f32 = 135.0;
    const SWEEP: f32 = 270.0;
    let track = arc(c, r, START, START + SWEEP);
    p.circle_filled(track[0], width / 2.0, TRACK);
    p.circle_filled(*track.last().expect("arc"), width / 2.0, TRACK);
    p.add(Shape::line(track, Stroke::new(width, TRACK)));
    let Some(f) = frac.map(|f| f.clamp(0.0, 1.0)) else { return };
    let end = START + SWEEP * f.max(0.004);
    let pts = arc(c, r, START, end);
    let color_at = move |pos: Pos2| {
        let deg = (pos.y - c.y).atan2(pos.x - c.x).to_degrees();
        lerp(from, to, (deg - START).rem_euclid(360.0) / SWEEP)
    };
    let (first, last) = (pts[0], *pts.last().expect("arc"));
    p.circle_filled(first, width / 2.0, from);
    p.circle_filled(last, width / 2.0, lerp(from, to, f));
    p.add(Shape::Path(PathShape {
        points: pts,
        closed: false,
        fill: Color32::TRANSPARENT,
        stroke: PathStroke { width, color: ColorMode::UV(Arc::new(move |_, pos| color_at(pos))), kind: StrokeKind::Middle },
    }));
    p.circle_filled(last, width / 2.0 - 3.5, Color32::WHITE);
}

/// Aire sous une courbe, en dégradé vertical vers le transparent.
fn area(p: &Painter, pts: &[Pos2], bottom: f32, color: Color32) {
    let mut mesh = Mesh::default();
    for (i, &pt) in pts.iter().enumerate() {
        mesh.colored_vertex(pt, color.gamma_multiply(0.32));
        mesh.colored_vertex(pos2(pt.x, bottom), color.gamma_multiply(0.02));
        if i > 0 {
            let b = i as u32 * 2;
            mesh.add_triangle(b - 2, b - 1, b);
            mesh.add_triangle(b - 1, b + 1, b);
        }
    }
    p.add(Shape::mesh(mesh));
}

/// Mini-graphe de l'historique (échelle automatique, marge de 10 %).
fn sparkline(p: &Painter, r: Rect, samples: &VecDeque<f32>, min_span: f32, color: Color32) {
    if samples.len() < 2 {
        p.text(r.center(), Align2::CENTER_CENTER, "Collecte…", regular(12.0), FAINT);
        return;
    }
    let (lo, hi) = samples.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    let mid = (lo + hi) / 2.0;
    let span = (hi - lo).max(min_span) * 1.3;
    let (lo, hi) = (mid - span / 2.0, mid + span / 2.0);
    let step = r.width() / (HISTORY - 1) as f32;
    let x0 = r.right() - step * (samples.len() - 1) as f32;
    let pts: Vec<Pos2> =
        samples.iter().enumerate().map(|(i, &v)| pos2(x0 + step * i as f32, r.bottom() - (v - lo) / (hi - lo) * r.height())).collect();
    area(p, &pts, r.bottom(), color);
    p.add(Shape::line(pts, Stroke::new(2.0_f32, color)));
}

/// Pictogrammes de la barre latérale, dessinés (pas de police d'icônes).
fn nav_icon(p: &Painter, c: Pos2, page: Page, color: Color32) {
    let s = Stroke::new(1.6_f32, color);
    match page {
        Page::Dashboard => {
            for (dx, dy) in [(-4.5, -4.5), (4.5, -4.5), (-4.5, 4.5), (4.5, 4.5)] {
                p.rect_stroke(Rect::from_center_size(c + vec2(dx, dy), vec2(6.5, 6.5)), 2, s, StrokeKind::Middle);
            }
        }
        Page::Cooling => {
            p.circle_stroke(c, 8.5, s);
            for k in 0..3 {
                let a = (k as f32 * 120.0 - 90.0).to_radians();
                let b = a + 0.9;
                let tip = c + vec2(a.cos(), a.sin()) * 6.5;
                let side = c + vec2(b.cos(), b.sin()) * 5.0;
                p.add(Shape::convex_polygon(vec![c, tip, side], color, Stroke::NONE));
            }
            p.circle_filled(c, 2.0, color);
        }
        Page::Lcd => {
            p.circle_stroke(c, 8.5, s);
            p.circle_stroke(c, 4.5, s);
        }
        Page::Settings => {
            for k in 0..8 {
                let a = (k as f32 * 45.0).to_radians();
                let d = vec2(a.cos(), a.sin());
                p.line_segment([c + d * 5.5, c + d * 8.5], Stroke::new(2.4_f32, color));
            }
            p.circle_stroke(c, 5.0, s);
            p.circle_filled(c, 1.8, color);
        }
    }
}

// ---------- application ----------

impl App {
    fn new(ctx: egui::Context) -> Self {
        style(&ctx);
        let (cfg, message) = match Config::load() {
            Ok(c) => (c, None),
            Err(e) => (Config::default(), Some((e, true))),
        };
        let image_text = cfg.lcd.image.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
        let mut app = Self {
            page: Page::Dashboard,
            channel: Channel::Pump,
            kraken: None,
            firmware: String::new(),
            status: None,
            sensors: Sensors::new(),
            readings: Readings::default(),
            history: History::default(),
            last_poll: Instant::now(),
            last_connect: Instant::now(),
            saved: cfg.clone(),
            cfg,
            applied: None,
            autostart: system::autostart_enabled(),
            cam_autostart: system::cam_autostart(),
            cam_running: false,
            daemon_running: daemon::is_running(),
            image_text,
            fonts: Fonts::load(),
            preview: None,
            preview_key: None,
            picture: None,
            message,
            icon_set: false,
        };
        app.connect();
        app
    }

    fn connect(&mut self) {
        self.last_connect = Instant::now();
        self.cam_running = !system::cam_processes().is_empty();
        match Kraken::open().and_then(|k| k.initialize().map(|fw| (k, fw))) {
            Ok((k, fw)) => {
                self.firmware = fw;
                self.kraken = Some(k);
                self.poll();
            }
            Err(e) => {
                self.kraken = None;
                if self.message.as_ref().is_none_or(|m| m.1) {
                    self.message = Some((e.to_string(), true));
                }
            }
        }
    }

    fn poll(&mut self) {
        self.last_poll = Instant::now();
        self.readings = self.sensors.read();
        self.daemon_running = daemon::is_running();
        if let Some(k) = &self.kraken {
            match k.status() {
                Ok(s) => self.status = Some(s),
                Err(e) => {
                    self.kraken = None;
                    self.status = None;
                    self.message = Some((e.to_string(), true));
                }
            }
        }
        let h = &mut self.history;
        h.cpu.push(self.readings.cpu_temp);
        h.gpu.push(self.readings.gpu_temp);
        h.liquid.push(self.status.map(|s| s.liquid_c));
        h.pump.push(self.status.map(|s| s.pump_rpm as f32));
        h.fan.push(self.status.map(|s| s.fan_rpm as f32));
    }

    fn values(&self) -> Values {
        Values { cpu: self.readings.cpu_temp, gpu: self.readings.gpu_temp, liquid: self.status.map(|s| s.liquid_c) }
    }

    fn dirty(&self) -> bool {
        self.cfg != self.saved || self.image_text.trim() != self.saved.lcd.image.as_ref().map(|p| p.display().to_string()).unwrap_or_default()
    }

    /// Sauvegarde config.toml. Le service résident est le seul à piloter la Kraken (l'écran passe
    /// par WinUSB, exclusif) : il relit le fichier tout seul, ou on le lance. Envoi direct en secours.
    fn apply(&mut self) {
        self.cfg.lcd.image = Some(self.image_text.trim().trim_matches('"')).filter(|s| !s.is_empty()).map(Into::into);
        if let Err(e) = self.cfg.save() {
            self.message = Some((format!("sauvegarde impossible : {e}"), true));
            return;
        }
        self.saved = self.cfg.clone();
        if daemon::is_running() {
            self.message = Some(("Appliqué par le service en fond".into(), false));
            return;
        }
        let spawn_error = match daemon::spawn() {
            Ok(()) => {
                self.daemon_running = true;
                self.message = Some(("Appliqué — service en fond lancé".into(), false));
                return;
            }
            Err(e) => e,
        };
        let Some(k) = self.kraken.as_mut() else {
            self.message = Some((format!("Sauvegardé ; service : {spawn_error}"), true));
            return;
        };
        let upload = self.applied.as_ref().is_none_or(|a| a.lcd != self.cfg.lcd);
        self.message = Some(match self.cfg.apply(k, upload) {
            Ok(()) => {
                self.applied = Some(self.cfg.clone());
                ("Appliqué directement (service indisponible)".into(), false)
            }
            Err(e) => (e, true),
        });
    }

    fn update_preview(&mut self, ctx: &egui::Context, size: u32) {
        let key = (self.values(), format!("{:?}", self.cfg.lcd), size);
        if self.preview.is_some() && self.preview_key.as_ref() == Some(&key) {
            return;
        }
        let color = round_image(&render::infographic(&self.fonts, &self.cfg.lcd, &key.0), size);
        match &mut self.preview {
            Some(t) => t.set(color, TextureOptions::LINEAR),
            None => self.preview = Some(ctx.load_texture("lcd", color, TextureOptions::LINEAR)),
        }
        self.preview_key = Some(key);
    }

    /// Première image du fichier choisi, réduite pour l'aperçu (rechargée si le chemin change).
    fn update_picture(&mut self, ctx: &egui::Context, size: u32) {
        let path = self.image_text.trim().trim_matches('"').to_owned();
        if self.picture.as_ref().is_some_and(|(p, s, _)| *p == path && *s == size) {
            return;
        }
        let tex = image::open(&path).ok().map(|img| {
            let img = img.resize_to_fill(kraken::LCD_SIZE, kraken::LCD_SIZE, image::imageops::FilterType::Triangle).to_rgba8();
            ctx.load_texture("picture", round_image(&img, size), TextureOptions::LINEAR)
        });
        self.picture = Some((path, size, tex));
    }
}

impl egui_software_backend::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _backend: &mut SoftwareBackend) {
        if !self.icon_set {
            // Icône de l'exe (celle de NZXT CAM) dans la barre de titre et la barre des tâches.
            if let Some(hwnd) = system::find_gui_window(Some(std::process::id())) {
                system::set_window_icons(hwnd);
                self.icon_set = true;
            }
        }
        if self.kraken.is_none() && self.last_connect.elapsed() >= RECONNECT {
            self.connect();
        } else if self.last_poll.elapsed() >= POLL {
            self.poll();
        }
        ui.ctx().request_repaint_after(POLL);
        if let Some(path) = ui.ctx().input(|i| i.raw.dropped_files.first().and_then(|f| f.path.clone())) {
            self.image_text = path.display().to_string();
            self.cfg.lcd.mode = LcdMode::Image;
            self.page = Page::Lcd;
        }
        let disc = match self.page {
            Page::Dashboard => Some(DASH_DISC),
            Page::Lcd => Some(LCD_DISC),
            _ => None,
        };
        if let Some(r) = disc {
            let size = (2.0 * r * ui.ctx().pixels_per_point()).round() as u32;
            match self.cfg.lcd.mode {
                LcdMode::Infographic => self.update_preview(ui.ctx(), size),
                LcdMode::Image => self.update_picture(ui.ctx(), size),
                LcdMode::Liquid => {}
            }
        }

        egui::CentralPanel::default().frame(egui::Frame::new().fill(BG)).show_inside(ui, |ui| {
            let full = ui.max_rect();
            let origin = full.min.to_vec2();
            self.sidebar(ui, Rect::from_min_size(full.min, vec2(SIDEBAR_W, full.height())));
            let content = Rect::from_min_max(pos2(SIDEBAR_W + PAD, TOP), pos2(WIDTH - PAD, HEIGHT - PAD)).translate(origin);
            self.header(ui, content);
            match self.page {
                Page::Dashboard => self.dashboard(ui, content),
                Page::Cooling => self.cooling(ui, content),
                Page::Lcd => self.lcd_page(ui, content),
                Page::Settings => self.settings(ui, content),
            }
        });
    }
}

// ---------- pages ----------

impl App {
    fn sidebar(&mut self, ui: &mut Ui, r: Rect) {
        let p = ui.painter().clone();
        p.rect_filled(r, 0, SIDEBAR);
        p.line_segment([r.right_top(), r.right_bottom()], Stroke::new(1.0_f32, BORDER));

        // Marque : pastille violette « N » (l'icône NZXT) + NZXT lite.
        let logo = rect(r.left() + 22.0, r.top() + 26.0, 34.0, 34.0);
        p.rect_filled(logo, 8, Color32::from_rgb(82, 0, 121));
        p.text(logo.center() + vec2(0.0, -1.0), Align2::CENTER_CENTER, "N", bold(21.0), Color32::WHITE);
        p.text(pos2(logo.right() + 12.0, logo.center().y - 1.0), Align2::LEFT_CENTER, "NZXT", bold(21.0), TEXT);
        p.text(pos2(logo.right() + 74.0, logo.center().y + 1.0), Align2::LEFT_CENTER, "lite", semibold(14.0), ACCENT_HI);

        let mut y = r.top() + 96.0;
        caption(&p, pos2(r.left() + 24.0, y - 22.0), "Menu");
        for (page, name) in [
            (Page::Dashboard, "Tableau de bord"),
            (Page::Cooling, "Refroidissement"),
            (Page::Lcd, "Écran"),
            (Page::Settings, "Paramètres"),
        ] {
            let item = rect(r.left() + 12.0, y, r.width() - 24.0, 42.0);
            let resp = ui.interact(item, ui.id().with(("nav", name)), Sense::click());
            hand_cursor(ui, &resp);
            let selected = self.page == page;
            if selected {
                p.rect_filled(item, 10, ACCENT.gamma_multiply(0.18));
                p.rect_filled(rect(item.left(), item.top() + 11.0, 3.0, 20.0), 2, ACCENT_HI);
            } else if resp.hovered() {
                p.rect_filled(item, 10, CARD);
            }
            let color = if selected { TEXT } else if resp.hovered() { TEXT } else { MUTED };
            nav_icon(&p, pos2(item.left() + 26.0, item.center().y), page, if selected { ACCENT_HI } else { color });
            p.text(pos2(item.left() + 48.0, item.center().y), Align2::LEFT_CENTER, name, semibold(14.0), color);
            if resp.clicked() {
                self.page = page;
            }
            y += 48.0;
        }

        // Appareil, en bas.
        let dev = rect(r.left() + 12.0, r.bottom() - 98.0, r.width() - 24.0, 84.0);
        p.rect(dev, 12, CARD, Stroke::new(1.0_f32, BORDER), StrokeKind::Inside);
        let disc = pos2(dev.left() + 30.0, dev.top() + 30.0);
        p.circle_filled(disc, 17.0, Color32::BLACK);
        p.circle_stroke(disc, 17.0, Stroke::new(2.0_f32, Color32::from_rgb(70, 70, 86)));
        p.circle_stroke(disc, 12.0, Stroke::new(1.2_f32, Color32::WHITE.gamma_multiply(0.7)));
        p.text(pos2(dev.left() + 56.0, dev.top() + 21.0), Align2::LEFT_CENTER, "Kraken Z", semibold(14.0), TEXT);
        let (color, text) = match (&self.kraken, self.cam_running) {
            (Some(_), _) => (OK, format!("Connectée · fw {}", self.firmware)),
            (None, true) => (WARN, "Bloquée par NZXT CAM".into()),
            (None, false) => (ERR, "Déconnectée".into()),
        };
        dot(&p, pos2(dev.left() + 60.0, dev.top() + 40.0), color);
        p.text(pos2(dev.left() + 70.0, dev.top() + 40.0), Align2::LEFT_CENTER, text, regular(12.0), MUTED);
        let (color, text) = if self.daemon_running { (OK, "Service en fond actif") } else { (FAINT, "Service en fond arrêté") };
        dot(&p, pos2(dev.left() + 18.0, dev.top() + 64.0), color);
        p.text(pos2(dev.left() + 28.0, dev.top() + 64.0), Align2::LEFT_CENTER, text, regular(12.0), MUTED);
    }

    fn header(&mut self, ui: &mut Ui, content: Rect) {
        let (title, subtitle) = match self.page {
            Page::Dashboard => ("Tableau de bord", "Températures et refroidissement en direct"),
            Page::Cooling => ("Refroidissement", "Courbes exécutées par la Kraken, selon la température du liquide"),
            Page::Lcd => ("Écran", "Écran rond 320 × 320 de la Kraken"),
            Page::Settings => ("Paramètres", "Démarrage, service en fond et capteurs"),
        };
        let p = ui.painter();
        let top = content.top() - 64.0;
        p.text(pos2(content.left(), top), Align2::LEFT_TOP, title, semibold(26.0), TEXT);
        p.text(pos2(content.left(), top + 38.0), Align2::LEFT_TOP, subtitle, regular(13.0), MUTED);
        if self.cam_running {
            let b = rect(content.right() - 150.0, top + 8.0, 150.0, 34.0);
            let r = chip(ui, b.left() - 10.0, b.center().y, "NZXT CAM bloque la Kraken", WARN);
            let _ = r;
            if button(ui, b, "close-cam", "Fermer NZXT CAM", Kind::Ghost, true) {
                match system::close_cam() {
                    Ok(()) => {
                        std::thread::sleep(Duration::from_millis(800));
                        self.message = None;
                        self.connect();
                    }
                    Err(e) => self.message = Some((e, true)),
                }
            }
        } else if self.page == Page::Cooling {
            let r = rect(content.right() - 248.0, top + 6.0, 248.0, 38.0);
            let selected = Some(if self.channel == Channel::Pump { 0 } else { 1 });
            match segmented(ui, r, "channel", &["Pompe", "Ventilateurs"], selected) {
                Some(0) => self.channel = Channel::Pump,
                Some(_) => self.channel = Channel::Fan,
                None => {}
            }
        }
    }

    fn dashboard(&mut self, ui: &mut Ui, c: Rect) {
        let p = ui.painter().clone();
        let w = (c.width() - 2.0 * GAP) / 3.0;
        let h1 = 262.0;
        let r = self.readings;
        let liquid = self.status.map(|s| s.liquid_c);
        let gauges = [
            ("Processeur", r.cpu_temp, &self.history.cpu, 20.0, 95.0, (ACCENT, PINK), r.cpu_load.map(|l| format!("Charge {l:.0} %"))),
            ("Carte graphique", r.gpu_temp, &self.history.gpu, 20.0, 90.0, (ACCENT, PINK), None),
            ("Liquide", liquid, &self.history.liquid, 20.0, 60.0, (BLUE, CYAN), liquid.map(|t| format!("{t:.1} °C"))),
        ];
        for (i, (name, temp, track, min, max, (from, to), sub)) in gauges.into_iter().enumerate() {
            let cr = rect(c.left() + i as f32 * (w + GAP), c.top(), w, h1);
            card(&p, cr);
            caption(&p, cr.min + vec2(20.0, 18.0), name);
            let center = pos2(cr.center().x, cr.top() + 128.0);
            gauge(&p, center, 66.0, 12.0, temp.map(|t| (t - min) / (max - min)), from, to);
            match temp {
                Some(t) => {
                    let v = p.layout_no_wrap(format!("{t:.0}"), semibold(44.0), TEXT);
                    let u = p.layout_no_wrap("°C".into(), regular(16.0), MUTED);
                    let x = center.x - (v.size().x + u.size().x + 2.0) / 2.0;
                    let y = center.y - v.size().y / 2.0 - 4.0;
                    p.galley(pos2(x + v.size().x + 2.0, y + 9.0), u, MUTED);
                    p.galley(pos2(x, y), v, TEXT);
                }
                None => {
                    p.text(center, Align2::CENTER_CENTER, "--", semibold(40.0), FAINT);
                }
            }
            let sub = sub.unwrap_or_else(|| if temp.is_some() { "Température".into() } else { "Indisponible".into() });
            p.text(center + vec2(0.0, 30.0), Align2::CENTER_CENTER, sub, regular(12.0), MUTED);
            // Extrêmes de la session.
            let fy = cr.bottom() - 26.0;
            p.line_segment([pos2(cr.left() + 20.0, fy - 18.0), pos2(cr.right() - 20.0, fy - 18.0)], Stroke::new(1.0_f32, BORDER));
            let (lo, hi) = track.range.map_or((None, None), |(lo, hi)| (Some(lo), Some(hi)));
            p.text(pos2(cr.left() + 20.0, fy), Align2::LEFT_CENTER, "Min", regular(12.0), MUTED);
            p.text(pos2(cr.left() + 48.0, fy), Align2::LEFT_CENTER, fmt_temp(lo), semibold(12.5), TEXT);
            p.text(pos2(cr.right() - 20.0, fy), Align2::RIGHT_CENTER, fmt_temp(hi), semibold(12.5), TEXT);
            let hw = text_width(ui, &fmt_temp(hi), semibold(12.5));
            p.text(pos2(cr.right() - 26.0 - hw, fy), Align2::RIGHT_CENTER, "Max", regular(12.0), MUTED);
        }

        let y2 = c.top() + h1 + GAP;
        let h2 = c.bottom() - y2;
        let s = self.status.unwrap_or_default();
        let connected = self.status.is_some();
        for (i, (name, channel, rpm, duty, track)) in [
            ("Pompe", Channel::Pump, s.pump_rpm, s.pump_duty, &self.history.pump),
            ("Ventilateurs", Channel::Fan, s.fan_rpm, s.fan_duty, &self.history.fan),
        ]
        .into_iter()
        .enumerate()
        {
            let cr = rect(c.left() + i as f32 * (w + GAP), y2, w, h2);
            card(&p, cr);
            caption(&p, cr.min + vec2(20.0, 18.0), name);
            let profile = match channel {
                Channel::Pump => &self.cfg.pump,
                Channel::Fan => &self.cfg.fan,
            };
            chip(ui, cr.right() - 16.0, cr.top() + 25.0, profile_name(profile, channel), ACCENT_HI);
            let v = p.layout_no_wrap(if connected { rpm.to_string() } else { "--".into() }, semibold(34.0), TEXT);
            let vw = v.size().x;
            p.galley(pos2(cr.left() + 20.0, cr.top() + 44.0), v, TEXT);
            p.text(pos2(cr.left() + 26.0 + vw, cr.top() + 72.0), Align2::LEFT_BOTTOM, "RPM", semibold(12.0), MUTED);
            p.text(pos2(cr.right() - 20.0, cr.top() + 72.0), Align2::RIGHT_BOTTOM, format!("{duty} %"), semibold(16.0), ACCENT_HI);
            let bar = rect(cr.left() + 20.0, cr.top() + 92.0, cr.width() - 40.0, 6.0);
            p.rect_filled(bar, 3, TRACK);
            p.rect_filled(rect(bar.left(), bar.top(), bar.width() * duty.min(100) as f32 / 100.0, 6.0), 3, ACCENT);
            sparkline(&p, rect(cr.left() + 20.0, cr.top() + 114.0, cr.width() - 40.0, cr.height() - 150.0), &track.samples, 300.0, ACCENT_HI);
            p.text(pos2(cr.left() + 20.0, cr.bottom() - 20.0), Align2::LEFT_CENTER, "2 dernières minutes", regular(11.5), FAINT);
        }

        // Écran : aperçu miniature.
        let cr = rect(c.left() + 2.0 * (w + GAP), y2, w, h2);
        card(&p, cr);
        caption(&p, cr.min + vec2(20.0, 18.0), "Écran");
        chip(ui, cr.right() - 16.0, cr.top() + 25.0, lcd_mode_name(self.cfg.lcd.mode), ACCENT_HI);
        self.lcd_disc(&p, pos2(cr.center().x, cr.top() + 48.0 + (h2 - 92.0) / 2.0), DASH_DISC);
        let info = format!("Luminosité {} % · {}°", self.cfg.lcd.brightness, self.cfg.lcd.rotation);
        p.text(pos2(cr.center().x, cr.bottom() - 20.0), Align2::CENTER_CENTER, info, regular(11.5), FAINT);
    }

    /// Écran rond avec son cerclage, tel qu'affiché par la Kraken (masque circulaire).
    fn lcd_disc(&self, p: &Painter, c: Pos2, r: f32) {
        const BEZEL: f32 = 10.0;
        let bezel = Color32::from_rgb(30, 30, 38);
        p.circle_filled(c, r + BEZEL, bezel);
        p.circle_filled(c, r, Color32::BLACK);
        let square = Rect::from_center_size(c, Vec2::splat(2.0 * r));
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        let texture = match self.cfg.lcd.mode {
            LcdMode::Infographic => self.preview.as_ref(),
            LcdMode::Image => self.picture.as_ref().and_then(|(_, _, t)| t.as_ref()),
            LcdMode::Liquid => None,
        };
        match (self.cfg.lcd.mode, texture) {
            (_, Some(tex)) => {
                p.image(tex.id(), square, uv, Color32::WHITE);
            }
            (LcdMode::Liquid, _) => {
                let k = r / 140.0;
                p.text(c - vec2(0.0, 46.0 * k), Align2::CENTER_CENTER, "LIQUIDE", semibold(15.0 * k), MUTED);
                p.text(c + vec2(0.0, 8.0 * k), Align2::CENTER_CENTER, fmt_temp(self.status.map(|s| s.liquid_c)), bold(76.0 * k), TEXT);
            }
            _ => {
                p.text(c, Align2::CENTER_CENTER, "Aucune image", regular(14.0 * r / 140.0), FAINT);
            }
        }
        p.circle_stroke(c, r + BEZEL, Stroke::new(1.0_f32, BORDER));
        p.circle_stroke(c, r, Stroke::new(1.0_f32, Color32::from_rgb(48, 48, 60)));
    }

    fn apply_bar(&mut self, ui: &mut Ui, c: Rect) {
        let y = c.bottom() - 40.0;
        let dirty = self.dirty();
        let apply = rect(c.right() - 140.0, y, 140.0, 40.0);
        let cancel = rect(apply.left() - 112.0, y, 100.0, 40.0);
        if button(ui, apply, "apply", "Appliquer", Kind::Primary, dirty || !self.daemon_running) {
            self.apply();
        }
        if button(ui, cancel, "cancel", "Annuler", Kind::Ghost, dirty) {
            self.cfg = self.saved.clone();
            self.image_text = self.cfg.lcd.image.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
        }
        let (color, text) = match (&self.message, dirty) {
            (_, true) => (WARN, "Modifications non appliquées".to_string()),
            (Some((m, err)), false) => (if *err { ERR } else { OK }, m.clone()),
            (None, false) => (FAINT, "Configuration à jour".to_string()),
        };
        let p = ui.painter();
        let cy = y + 20.0;
        dot(p, pos2(c.left() + 6.0, cy), color);
        let max_w = cancel.left() - c.left() - 30.0;
        let galley = p.layout(text, regular(13.0), if color == FAINT { MUTED } else { TEXT }, max_w);
        p.galley(pos2(c.left() + 18.0, cy - galley.size().y / 2.0), galley, TEXT);
    }

    fn cooling(&mut self, ui: &mut Ui, c: Rect) {
        let main = Rect::from_min_max(c.min, pos2(c.right(), c.bottom() - 56.0));
        let p = ui.painter().clone();
        card(&p, main);
        let channel = self.channel;
        let s = self.status.unwrap_or_default();
        let (rpm, duty) = match channel {
            Channel::Pump => (s.pump_rpm, s.pump_duty),
            Channel::Fan => (s.fan_rpm, s.fan_duty),
        };
        let liquid = self.status.map(|s| s.liquid_c);
        let profile = match channel {
            Channel::Pump => &mut self.cfg.pump,
            Channel::Fan => &mut self.cfg.fan,
        };

        // Préréglages.
        let silent = Profile::silent(channel);
        let perf = Profile::performance(channel);
        let is = |p: &Profile, q: &Profile| p.mode == Mode::Curve && p.curve == q.curve;
        let custom = profile.mode == Mode::Curve && !is(profile, &silent) && !is(profile, &perf);
        let items = [
            ("Silencieux", is(profile, &silent)),
            ("Performance", is(profile, &perf)),
            ("Fixe", profile.mode == Mode::Fixed),
            ("Personnalisé", custom),
        ];
        match pills(ui, main.min + vec2(20.0, 20.0), ("preset", channel as u8), &items) {
            Some(0) => *profile = silent.clone(),
            Some(1) => *profile = perf.clone(),
            Some(2) => profile.mode = Mode::Fixed,
            Some(3) if profile.mode == Mode::Fixed => profile.mode = Mode::Curve,
            _ => {}
        }

        // Mesures en direct, à droite.
        let mut x = main.right() - 20.0;
        for (label, value, color) in [
            ("Vitesse", format!("{duty} %"), ACCENT_HI),
            ("Régime", format!("{rpm} RPM"), TEXT),
            ("Liquide", liquid.map_or("--".into(), |t| format!("{t:.1} °C")), CYAN),
        ] {
            let vw = text_width(ui, &value, semibold(16.0));
            p.text(pos2(x, main.top() + 22.0), Align2::RIGHT_TOP, label, regular(11.5), MUTED);
            p.text(pos2(x, main.top() + 37.0), Align2::RIGHT_TOP, &value, semibold(16.0), color);
            x -= vw.max(48.0) + 26.0;
        }

        let body = Rect::from_min_max(main.min + vec2(20.0, 76.0), main.max - vec2(20.0, 40.0));
        match profile.mode {
            Mode::Curve => {
                curve_editor(ui, body, channel, &mut profile.curve, liquid);
                p.text(
                    pos2(main.left() + 20.0, main.bottom() - 20.0),
                    Align2::LEFT_CENTER,
                    "Glisser un point · double-clic pour en ajouter · clic droit pour supprimer",
                    regular(12.0),
                    FAINT,
                );
                p.text(
                    pos2(main.right() - 20.0, main.bottom() - 20.0),
                    Align2::RIGHT_CENTER,
                    format!("100 % imposé à {CRITICAL_TEMP} °C (sécurité)"),
                    regular(12.0),
                    FAINT,
                );
            }
            Mode::Fixed => {
                let center = pos2(body.center().x, body.top() + 140.0);
                gauge(&p, center, 96.0, 14.0, Some(profile.fixed as f32 / 100.0), ACCENT, PINK);
                p.text(center - vec2(0.0, 6.0), Align2::CENTER_CENTER, format!("{} %", profile.fixed), semibold(44.0), TEXT);
                p.text(center + vec2(0.0, 32.0), Align2::CENTER_CENTER, "Vitesse constante", regular(12.5), MUTED);
                let sr = Rect::from_center_size(pos2(body.center().x, body.top() + 290.0), vec2(420.0, 28.0));
                let min = channel.min_duty();
                slider(ui, sr, ("fixed", channel as u8), &mut profile.fixed, min, 100);
                p.text(pos2(sr.left() + 10.0, sr.bottom() + 12.0), Align2::LEFT_CENTER, format!("{min} %"), regular(11.5), FAINT);
                p.text(pos2(sr.right() - 10.0, sr.bottom() + 12.0), Align2::RIGHT_CENTER, "100 %", regular(11.5), FAINT);
            }
        }
        self.apply_bar(ui, c);
    }

    fn lcd_page(&mut self, ui: &mut Ui, c: Rect) {
        let p = ui.painter().clone();
        let body_bottom = c.bottom() - 56.0;
        let left = Rect::from_min_max(c.min, pos2(c.left() + 324.0, body_bottom));
        let right = Rect::from_min_max(pos2(left.right() + GAP, c.top()), pos2(c.right(), body_bottom));
        card(&p, left);
        card(&p, right);

        caption(&p, left.min + vec2(20.0, 18.0), "Aperçu");
        chip(ui, left.right() - 16.0, left.top() + 25.0, if self.daemon_running { "En direct" } else { "Aperçu" }, OK);
        self.lcd_disc(&p, pos2(left.center().x, (left.top() + 48.0 + left.bottom() - 64.0) / 2.0), LCD_DISC);
        let note = match self.cfg.lcd.mode {
            LcdMode::Infographic => "Redessiné par le service quand une valeur change",
            LcdMode::Liquid => "Affichage intégré au firmware de la Kraken",
            LcdMode::Image => "Envoyé une fois ; l'écran joue le GIF lui-même",
        };
        let g = p.layout(note.into(), regular(12.0), MUTED, left.width() - 40.0);
        p.galley(pos2(left.center().x - g.size().x / 2.0, left.bottom() - 40.0), g, MUTED);

        let x = right.left() + 20.0;
        let w = right.width() - 40.0;
        let mut y = right.top() + 18.0;
        caption(&p, pos2(x, y), "Mode");
        y += 24.0;
        let lcd = &mut self.cfg.lcd;
        let modes = [LcdMode::Infographic, LcdMode::Liquid, LcdMode::Image];
        let current = modes.iter().position(|m| *m == lcd.mode);
        if let Some(i) = segmented(ui, rect(x, y, w, 38.0), "mode", &["Infographie", "Liquide", "Image / GIF"], current) {
            lcd.mode = modes[i];
        }
        y += 58.0;
        match lcd.mode {
            LcdMode::Infographic => {
                caption(&p, pos2(x, y), "Valeurs affichées");
                y += 24.0;
                for (i, slot) in ["Gauche", "Droite", "Bas"].iter().enumerate() {
                    p.text(pos2(x, y + 16.0), Align2::LEFT_CENTER, *slot, regular(13.0), MUTED);
                    let current = Reading::ALL.iter().position(|r| *r == lcd.readings[i]);
                    let labels = Reading::ALL.map(Reading::label);
                    if let Some(k) = segmented(ui, rect(x + 72.0, y, w - 72.0, 32.0), ("slot", i), &labels, current) {
                        lcd.readings[i] = Reading::ALL[k];
                    }
                    y += 40.0;
                }
                y += 4.0;
                p.text(pos2(x, y + 12.0), Align2::LEFT_CENTER, "Logo NZXT", regular(13.5), TEXT);
                toggle(ui, rect(x + w - 44.0, y, 44.0, 24.0), "logo", &mut lcd.logo);
                y += 40.0;
            }
            LcdMode::Liquid => {
                let g = p.layout(
                    "La Kraken affiche elle-même la température du liquide : rien ne tourne en fond pour l'écran.".into(),
                    regular(13.0),
                    MUTED,
                    w,
                );
                let h = g.size().y;
                p.galley(pos2(x, y), g, MUTED);
                y += h + 24.0;
            }
            LcdMode::Image => {
                caption(&p, pos2(x, y), "Fichier");
                y += 24.0;
                ui.put(
                    rect(x, y, w, 36.0),
                    egui::TextEdit::singleline(&mut self.image_text)
                        .font(regular(13.0))
                        .margin(vec2(10.0, 9.0))
                        .hint_text("Chemin d'une image ou d'un GIF"),
                );
                y += 44.0;
                p.text(pos2(x, y), Align2::LEFT_TOP, "Ou glisse le fichier sur la fenêtre.", regular(12.0), FAINT);
                y += 32.0;
            }
        }
        let lcd = &mut self.cfg.lcd;
        caption(&p, pos2(x, y), "Luminosité");
        p.text(pos2(x + w, y - 2.0), Align2::RIGHT_TOP, format!("{} %", lcd.brightness), semibold(13.0), TEXT);
        y += 22.0;
        slider(ui, rect(x - 10.0, y, w + 20.0, 28.0), "brightness", &mut lcd.brightness, 0, 100);
        y += 44.0;
        caption(&p, pos2(x, y), "Rotation");
        y += 24.0;
        let rotations = [0u16, 90, 180, 270];
        let current = rotations.iter().position(|r| *r == lcd.rotation);
        if let Some(i) = segmented(ui, rect(x, y, w, 34.0), "rotation", &["0°", "90°", "180°", "270°"], current) {
            lcd.rotation = rotations[i];
        }
        self.apply_bar(ui, c);
    }

    fn settings(&mut self, ui: &mut Ui, c: Rect) {
        let p = ui.painter().clone();
        let w = (c.width() - GAP) / 2.0;
        let h = (c.height() - GAP) / 2.0;
        let cards = [0, 1, 2, 3].map(|i| rect(c.left() + (i % 2) as f32 * (w + GAP), c.top() + (i / 2) as f32 * (h + GAP), w, h));
        for r in cards {
            card(&p, r);
        }
        let para = |r: Rect, y: f32, text: &str| {
            let g = p.layout(text.into(), regular(12.5), MUTED, r.width() - 40.0);
            p.galley(pos2(r.left() + 20.0, y), g, MUTED);
        };

        // Démarrage.
        let r = cards[0];
        caption(&p, r.min + vec2(20.0, 18.0), "Démarrage");
        p.text(pos2(r.left() + 20.0, r.top() + 62.0), Align2::LEFT_CENTER, "Lancer au démarrage", semibold(15.0), TEXT);
        if toggle(ui, rect(r.right() - 64.0, r.top() + 50.0, 44.0, 24.0), "autostart", &mut self.autostart) {
            if let Err(e) = system::set_autostart(self.autostart) {
                self.autostart = !self.autostart;
                self.message = Some((e, true));
            }
        }
        para(
            r,
            r.top() + 86.0,
            "Démarre le service en fond à l'ouverture de session : icône de notification, courbes réappliquées, infographie sur l'écran.",
        );
        // NZXT CAM relancé à l'ouverture de session reprendrait la Kraken en exclusif.
        let y = r.bottom() - 40.0;
        p.line_segment([pos2(r.left() + 20.0, y - 30.0), pos2(r.right() - 20.0, y - 30.0)], Stroke::new(1.0_f32, BORDER));
        p.text(pos2(r.left() + 20.0, y - 9.0), Align2::LEFT_CENTER, "NZXT CAM au démarrage", semibold(13.5), TEXT);
        let (color, text) =
            if self.cam_autostart { (WARN, "Présent : il reprendrait la Kraken") } else { (OK, "Absent") };
        p.text(pos2(r.left() + 20.0, y + 10.0), Align2::LEFT_CENTER, text, regular(12.0), color);
        if self.cam_autostart {
            let b = rect(r.right() - 100.0, y - 16.0, 80.0, 32.0);
            if button(ui, b, "cam-autostart", "Retirer", Kind::Ghost, true) {
                if let Err(e) = system::remove_cam_autostart() {
                    self.message = Some((e, true));
                }
                self.cam_autostart = system::cam_autostart();
            }
        }

        // Service.
        let r = cards[1];
        caption(&p, r.min + vec2(20.0, 18.0), "Service en fond");
        let (color, text) = if self.daemon_running { (OK, "Actif") } else { (FAINT, "Arrêté") };
        dot(&p, pos2(r.left() + 25.0, r.top() + 62.0), color);
        p.text(pos2(r.left() + 36.0, r.top() + 62.0), Align2::LEFT_CENTER, text, semibold(15.0), TEXT);
        let b = rect(r.right() - 120.0, r.top() + 44.0, 100.0, 36.0);
        let label = if self.daemon_running { "Arrêter" } else { "Démarrer" };
        if button(ui, b, "service", label, if self.daemon_running { Kind::Ghost } else { Kind::Primary }, true) {
            let result = if self.daemon_running { daemon::stop() } else { daemon::spawn() };
            match result {
                Ok(()) => {
                    std::thread::sleep(Duration::from_millis(300));
                    self.daemon_running = daemon::is_running();
                }
                Err(e) => self.message = Some((e, true)),
            }
        }
        para(
            r,
            r.top() + 96.0,
            "Petit processus résident (~4 Mo) : icône près de l'horloge avec les températures, réapplication après la veille ou une modification, dessin de l'infographie.",
        );

        // Capteurs.
        let r = cards[2];
        caption(&p, r.min + vec2(20.0, 18.0), "Capteurs");
        let rows = [
            ("CPU", "Tctl · pilote WinRing0", self.readings.cpu_temp),
            ("GPU", "D3DKMT (noyau Windows)", self.readings.gpu_temp),
            ("Liquide", "Kraken (HID)", self.status.map(|s| s.liquid_c)),
        ];
        for (i, (name, source, value)) in rows.into_iter().enumerate() {
            let y = r.top() + 62.0 + i as f32 * 46.0;
            dot(&p, pos2(r.left() + 25.0, y), if value.is_some() { OK } else { ERR });
            p.text(pos2(r.left() + 38.0, y - 8.0), Align2::LEFT_CENTER, name, semibold(14.0), TEXT);
            p.text(pos2(r.left() + 38.0, y + 10.0), Align2::LEFT_CENTER, source, regular(12.0), MUTED);
            let v = value.map_or("indisponible".into(), |t| format!("{t:.1} °C"));
            p.text(pos2(r.right() - 20.0, y), Align2::RIGHT_CENTER, v, semibold(14.0), if value.is_some() { TEXT } else { FAINT });
        }

        // Appareil & configuration.
        let r = cards[3];
        caption(&p, r.min + vec2(20.0, 18.0), "Appareil");
        let fw = if self.firmware.is_empty() { "--".to_string() } else { self.firmware.clone() };
        for (i, (k, v)) in [("Modèle", "NZXT Kraken Z (1E71:3008)".to_string()), ("Firmware", fw)].into_iter().enumerate() {
            let y = r.top() + 58.0 + i as f32 * 26.0;
            p.text(pos2(r.left() + 20.0, y), Align2::LEFT_CENTER, k, regular(13.0), MUTED);
            p.text(pos2(r.right() - 20.0, y), Align2::RIGHT_CENTER, v, semibold(13.0), TEXT);
        }
        let path = crate::config::path();
        let g = p.layout(path.display().to_string(), regular(12.0), FAINT, r.width() - 40.0);
        p.galley(pos2(r.left() + 20.0, r.top() + 118.0), g, FAINT);
        let b = rect(r.left() + 20.0, r.bottom() - 56.0, 170.0, 36.0);
        if button(ui, b, "open-config", "Ouvrir le dossier", Kind::Ghost, true) {
            if let Some(dir) = path.parent() {
                let _ = std::process::Command::new("explorer").arg(dir).spawn();
            }
        }
        if let Some((msg, true)) = &self.message {
            let g = p.layout(msg.clone(), regular(12.0), ERR, r.width() - 230.0);
            p.galley(pos2(b.right() + 14.0, b.center().y - g.size().y / 2.0), g, ERR);
        }
    }
}

fn profile_name(profile: &Profile, channel: Channel) -> &'static str {
    match profile.mode {
        Mode::Fixed => "Fixe",
        Mode::Curve if profile.curve == Profile::silent(channel).curve => "Silencieux",
        Mode::Curve if profile.curve == Profile::performance(channel).curve => "Performance",
        Mode::Curve => "Personnalisé",
    }
}

fn lcd_mode_name(mode: LcdMode) -> &'static str {
    match mode {
        LcdMode::Infographic => "Infographie",
        LcdMode::Liquid => "Liquide",
        LcdMode::Image => "Image / GIF",
    }
}

const T_MIN: f32 = CURVE_MIN_TEMP as f32;
const T_MAX: f32 = CRITICAL_TEMP as f32;

/// Éditeur de courbe : glisser un point, double-clic pour ajouter, clic droit pour supprimer.
fn curve_editor(ui: &mut Ui, r: Rect, channel: Channel, points: &mut Vec<(u8, u8)>, live_temp: Option<f32>) {
    let bg = ui.interact(r, ui.id().with(("curve", channel as u8)), Sense::click());
    let plot = Rect::from_min_max(r.min + vec2(44.0, 10.0), r.max - vec2(8.0, 26.0));
    let to_screen =
        |t: f32, d: f32| pos2(plot.left() + (t - T_MIN) / (T_MAX - T_MIN) * plot.width(), plot.bottom() - d / 100.0 * plot.height());
    let from_screen = |p: Pos2| {
        let t = T_MIN + (p.x - plot.left()) / plot.width() * (T_MAX - T_MIN);
        let d = (plot.bottom() - p.y) / plot.height() * 100.0;
        (t.round().clamp(T_MIN, T_MAX - 1.0) as u8, d.round().clamp(channel.min_duty() as f32, 100.0) as u8)
    };

    let painter = ui.painter_at(r.expand(12.0));
    painter.rect_filled(plot, 8, FIELD);
    let grid = Stroke::new(1.0_f32, Color32::from_rgb(30, 30, 40));
    for d in (0..=100).step_by(20) {
        let y = to_screen(T_MIN, d as f32).y;
        painter.line_segment([pos2(plot.left(), y), pos2(plot.right(), y)], grid);
        painter.text(pos2(plot.left() - 10.0, y), Align2::RIGHT_CENTER, format!("{d} %"), regular(11.0), FAINT);
    }
    for t in (20..=55).step_by(5) {
        let x = to_screen(t as f32, 0.0).x;
        painter.line_segment([pos2(x, plot.top()), pos2(x, plot.bottom())], grid);
        painter.text(pos2(x, plot.bottom() + 8.0), Align2::CENTER_TOP, format!("{t}°"), regular(11.0), FAINT);
    }
    // Zone critique : la Kraken force 100 % à 59 °C.
    let xc = to_screen(T_MAX, 0.0).x;
    painter.line_segment([pos2(xc, plot.top()), pos2(xc, plot.bottom())], Stroke::new(1.5_f32, ERR.gamma_multiply(0.6)));
    painter.text(pos2(xc, plot.bottom() + 8.0), Align2::CENTER_TOP, format!("{CRITICAL_TEMP}°"), semibold(11.0), ERR);

    // Courbe réellement envoyée (40 valeurs, après interpolation et sécurité 59 °C).
    let duties = kraken::interpolate(points, channel);
    let line: Vec<Pos2> = duties.iter().enumerate().map(|(i, &d)| to_screen(T_MIN + i as f32, d as f32)).collect();
    area(&painter, &line, plot.bottom(), ACCENT);
    painter.add(Shape::line(line, Stroke::new(2.5_f32, ACCENT_HI)));

    if let Some(t) = live_temp.filter(|t| (T_MIN..=T_MAX).contains(t)) {
        let i = ((t - T_MIN).round() as usize).min(duties.len() - 1);
        let at = to_screen(t, duties[i] as f32);
        let mut y = plot.top();
        while y < plot.bottom() {
            painter.line_segment([pos2(at.x, y), pos2(at.x, (y + 4.0).min(plot.bottom()))], Stroke::new(1.5_f32, CYAN.gamma_multiply(0.8)));
            y += 8.0;
        }
        painter.circle(at, 5.0, CYAN, Stroke::new(2.0_f32, FIELD));
        let label = format!("{t:.1} °C · {} %", duties[i]);
        let g = painter.layout_no_wrap(label, semibold(11.5), FIELD);
        let lw = g.size().x + 16.0;
        let lx = (at.x - lw / 2.0).clamp(plot.left() + 4.0, plot.right() - lw - 4.0);
        let lr = rect(lx, plot.top() + 6.0, lw, 22.0);
        painter.rect_filled(lr, 11, CYAN);
        painter.galley(pos2(lr.left() + 8.0, lr.center().y - g.size().y / 2.0), g, FIELD);
    }

    let mut remove = None;
    for i in 0..points.len() {
        // Un point reste entre ses voisins : l'ordre (et donc l'id de drag) ne change jamais.
        let lo = if i == 0 { CURVE_MIN_TEMP } else { points[i - 1].0 + 1 };
        let hi = points.get(i + 1).map_or(CRITICAL_TEMP - 1, |n| n.0 - 1);
        let pt = &mut points[i];
        let center = to_screen(pt.0 as f32, pt.1 as f32);
        let id = ui.id().with(("pt", channel as u8, i));
        let resp = ui.interact(Rect::from_center_size(center, Vec2::splat(18.0)), id, Sense::click_and_drag());
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::Grab);
        }
        if resp.dragged() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let (t, d) = from_screen(pos);
                *pt = (t.clamp(lo, hi.max(lo)), d);
            }
        }
        if resp.secondary_clicked() {
            remove = Some(i);
        }
        let hot = resp.hovered() || resp.dragged();
        let center = to_screen(pt.0 as f32, pt.1 as f32);
        if hot {
            painter.circle_filled(center, 13.0, ACCENT.gamma_multiply(0.3));
        }
        painter.circle(center, if hot { 7.0 } else { 6.0 }, Color32::WHITE, Stroke::new(3.0_f32, ACCENT));
        if hot {
            let label = format!("{}° · {} %", pt.0, pt.1);
            let g = painter.layout_no_wrap(label, semibold(12.0), TEXT);
            let lr = Rect::from_center_size(center - vec2(0.0, 26.0), g.size() + vec2(16.0, 8.0));
            painter.rect(lr, 7, CARD_HI, Stroke::new(1.0_f32, BORDER), StrokeKind::Inside);
            painter.galley(lr.min + vec2(8.0, 4.0), g, TEXT);
        }
    }
    if let Some(i) = remove.filter(|_| points.len() > 2) {
        points.remove(i);
    }
    if bg.double_clicked() {
        if let Some(pos) = bg.interact_pointer_pos().filter(|p| plot.contains(*p)) {
            let new = from_screen(pos);
            if !points.iter().any(|p| p.0 == new.0) {
                points.push(new);
            }
        }
    }
    points.sort_by_key(|p| p.0);
}
