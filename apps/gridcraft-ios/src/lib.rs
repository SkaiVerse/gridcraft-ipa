//! GridCraft for iPad: the engine and egui interface, drawn with Metal, driven by a Swift shell.
#![allow(clippy::missing_safety_doc)]

use std::cell::RefCell;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

use gridcraft_engine::Session;
use gridcraft_ui_egui::{Inbox, Services, SheetApp};

// ---------------------------------------------------------------------------------------------
// Callbacks into Swift (must match GridCraft-Bridging-Header.h, field for field)
// ---------------------------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GCCallbacks {
    pub user: *mut c_void,
    pub open_files: Option<unsafe extern "C" fn(*mut c_void)>,
    pub save_file: Option<unsafe extern "C" fn(*mut c_void, *const c_char, *const u8, usize)>,
    pub open_url: Option<unsafe extern "C" fn(*mut c_void, *const c_char)>,
    pub copy_text: Option<unsafe extern "C" fn(*mut c_void, *const c_char)>,
    pub copy_html: Option<unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char)>,
    pub paste_text: Option<unsafe extern "C" fn(*mut c_void) -> *const c_char>,
    pub paste_html: Option<unsafe extern "C" fn(*mut c_void) -> *const c_char>,
    pub pick_picture: Option<unsafe extern "C" fn(*mut c_void)>,
    pub print_pdf: Option<unsafe extern "C" fn(*mut c_void, *const u8, usize)>,
}

// ---------------------------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------------------------

fn documents_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Documents"))
}

fn note(msg: &str) {
    use std::io::Write;
    let Some(dir) = documents_dir() else { return };
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("GridCraft-log.txt")) {
        let _ = writeln!(f, "[rust] {msg}");
    }
}

fn cstring(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

unsafe fn from_c(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

fn guard<R>(default: R, f: impl FnOnce() -> R) -> R {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(default)
}

fn to_mods(m: u32) -> egui::Modifiers {
    // Only Apple's modifier rules: Command is the shortcut key, Control stays Control.
    let cmd = m & 8 != 0;
    egui::Modifiers { alt: m & 4 != 0, ctrl: m & 2 != 0, shift: m & 1 != 0, mac_cmd: cmd, command: cmd }
}

fn hid_to_key(hid: u32) -> Option<egui::Key> {
    use egui::Key;
    const LETTERS: [Key; 26] = [
        Key::A, Key::B, Key::C, Key::D, Key::E, Key::F, Key::G, Key::H, Key::I, Key::J, Key::K, Key::L, Key::M,
        Key::N, Key::O, Key::P, Key::Q, Key::R, Key::S, Key::T, Key::U, Key::V, Key::W, Key::X, Key::Y, Key::Z,
    ];
    const DIGITS: [Key; 9] =
        [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
    const FKEYS: [Key; 12] =
        [Key::F1, Key::F2, Key::F3, Key::F4, Key::F5, Key::F6, Key::F7, Key::F8, Key::F9, Key::F10, Key::F11, Key::F12];
    Some(match hid {
        0x04..=0x1D => LETTERS[(hid - 0x04) as usize],
        0x1E..=0x26 => DIGITS[(hid - 0x1E) as usize],
        0x27 => Key::Num0,
        0x28 => Key::Enter,
        0x29 => Key::Escape,
        0x2A => Key::Backspace,
        0x2B => Key::Tab,
        0x2C => Key::Space,
        0x2D => Key::Minus,
        0x2E => Key::Equals,
        0x2F => Key::OpenBracket,
        0x30 => Key::CloseBracket,
        0x31 => Key::Backslash,
        0x33 => Key::Semicolon,
        0x34 => Key::Quote,
        0x35 => Key::Backtick,
        0x36 => Key::Comma,
        0x37 => Key::Period,
        0x38 => Key::Slash,
        0x3A..=0x45 => FKEYS[(hid - 0x3A) as usize],
        0x49 => Key::Insert,
        0x4A => Key::Home,
        0x4B => Key::PageUp,
        0x4C => Key::Delete,
        0x4D => Key::End,
        0x4E => Key::PageDown,
        0x4F => Key::ArrowRight,
        0x50 => Key::ArrowLeft,
        0x51 => Key::ArrowDown,
        0x52 => Key::ArrowUp,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------------------------
// Preferences (same files as the desktop app, in the app's own folder)
// ---------------------------------------------------------------------------------------------

fn config_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support/GridCraft"))
}

fn load_prefs(app: &mut SheetApp) {
    app.ui.language = gridcraft_ui_egui::i18n::Language::system();
    app.ui.system_theme = true;
    let Some(dir) = config_dir() else { return };
    if let Ok(b) = std::fs::read(dir.join("ui.json"))
        && let Ok(ui) = serde_json::from_slice::<gridcraft_ui_egui::UiState>(&b)
    {
        let saved = serde_json::from_slice(&b).ok().and_then(|v| gridcraft_ui_egui::i18n::saved_language(&v));
        let language = saved.unwrap_or(app.ui.language);
        app.ui = ui.at_startup();
        app.ui.language = language;
    }
    if let Ok(b) = std::fs::read(dir.join("prefs.json"))
        && let Ok(p) = serde_json::from_slice(&b)
    {
        app.session.prefs = p;
    }
}

fn save_prefs(app: &SheetApp) {
    let Some(dir) = config_dir() else { return };
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(b) = serde_json::to_vec_pretty(&app.ui) {
        let _ = std::fs::write(dir.join("ui.json"), b);
    }
    if let Ok(b) = serde_json::to_vec_pretty(&app.session.prefs) {
        let _ = std::fs::write(dir.join("prefs.json"), b);
    }
}

// ---------------------------------------------------------------------------------------------
// Services: what the app asks the iPad to do
// ---------------------------------------------------------------------------------------------

fn build_services(
    cb: GCCallbacks,
    inbox: &Inbox,
    pictures: &Rc<RefCell<Option<Inbox>>>,
    pending: &Rc<RefCell<Option<String>>>,
    pending_bytes: &Rc<RefCell<Option<Vec<u8>>>>,
) -> Services {
    let pictures = pictures.clone();
    let pending = pending.clone();
    let pending_bytes = pending_bytes.clone();
    Services {
        inbox: Some(inbox.clone()),
        open_async: Some(Box::new(move || {
            note("the app asked to open a file");
            if let Some(f) = cb.open_files {
                unsafe { f(cb.user) }
            }
        })),
        pick_picture_async: Some(Box::new(move |target: Inbox| {
            *pictures.borrow_mut() = Some(target);
            if let Some(f) = cb.pick_picture {
                unsafe { f(cb.user) }
            }
        })),
        download: Some(Box::new(move |name: &str, bytes: &[u8]| {
            *pending.borrow_mut() = Some(name.to_string());
            *pending_bytes.borrow_mut() = Some(bytes.to_vec());
            if let Some(f) = cb.save_file {
                let n = cstring(name);
                unsafe { f(cb.user, n.as_ptr(), bytes.as_ptr(), bytes.len()) }
            }
        })),
        print: Some(Box::new(move |bytes: &[u8]| {
            if let Some(f) = cb.print_pdf {
                unsafe { f(cb.user, bytes.as_ptr(), bytes.len()) }
            }
        })),
        open_url: Some(Box::new(move |url: &str| {
            let gridcraft_engine::links::LinkKind::External(target) = gridcraft_engine::links::classify(url) else {
                return;
            };
            if let Some(f) = cb.open_url {
                let c = cstring(target.as_str());
                unsafe { f(cb.user, c.as_ptr()) }
            }
        })),
        copy_html: Some(Box::new(move |html: &str, text: &str| {
            let Some(f) = cb.copy_html else { return Err("no clipboard".to_string()) };
            let (h, t) = (cstring(html), cstring(text));
            unsafe { f(cb.user, h.as_ptr(), t.as_ptr()) };
            Ok(())
        })),
        paste_html: Some(Box::new(move || {
            let f = cb.paste_html?;
            unsafe { from_c(f(cb.user)) }
        })),
        paste_text: Some(Box::new(move || {
            let f = cb.paste_text?;
            unsafe { from_c(f(cb.user)) }
        })),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------------------------
// Graphics (Metal through wgpu)
// ---------------------------------------------------------------------------------------------

struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: egui_wgpu::Renderer,
    config: wgpu::SurfaceConfiguration,
    needs_reconfigure: bool,
}

impl Gpu {
    fn new(layer: *mut c_void, width: u32, height: u32) -> Result<Gpu, String> {
        let instance = wgpu::Instance::default();
        let surface: wgpu::Surface<'static> =
            unsafe { instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::CoreAnimationLayer(layer)) }
                .map_err(|e| format!("surface: {e}"))?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .map_err(|e| format!("adapter: {e}"))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("gridcraft"),
            required_limits: wgpu::Limits { max_texture_dimension_2d: 8192, ..wgpu::Limits::default() },
            ..Default::default()
        }))
        .map_err(|e| format!("device: {e}"))?;
        let caps = surface.get_capabilities(&adapter);
        let format = egui_wgpu::preferred_framebuffer_format(&caps.formats).map_err(|e| format!("format: {e}"))?;
        let mut config = surface
            .get_default_config(&adapter, width.max(1), height.max(1))
            .ok_or_else(|| "this surface is not supported".to_string())?;
        config.format = format;
        config.view_formats = vec![format];
        config.present_mode = wgpu::PresentMode::AutoVsync;
        surface.configure(&device, &config);
        let renderer = egui_wgpu::Renderer::new(&device, format, egui_wgpu::RendererOptions::default());
        Ok(Gpu { surface, device, queue, renderer, config, needs_reconfigure: false })
    }
}

// ---------------------------------------------------------------------------------------------
// The host: one per window
// ---------------------------------------------------------------------------------------------

struct Host {
    app: SheetApp,
    ctx: egui::Context,
    gpu: Gpu,
    scale: f32,
    events: Vec<egui::Event>,
    modifiers: egui::Modifiers,
    dark: bool,
    focused: bool,
    start: Instant,
    deadline: Arc<Mutex<Option<Instant>>>,
    cb: GCCallbacks,
    inbox: Inbox,
    pictures: Rc<RefCell<Option<Inbox>>>,
    pending_save: Rc<RefCell<Option<String>>>,
    pending_bytes: Rc<RefCell<Option<Vec<u8>>>>,
    wants_keyboard: bool,
    zoom_pct: f32,
    fonts_logged: bool,
}

fn wake_at(deadline: &Arc<Mutex<Option<Instant>>>, at: Instant) {
    let mut d = deadline.lock().unwrap_or_else(|e| e.into_inner());
    if d.is_none_or(|cur| at < cur) {
        *d = Some(at);
    }
}

impl Host {
    fn new(layer: *mut c_void, width: u32, height: u32, scale: f32, cb: GCCallbacks) -> Result<Host, String> {
        // Folders that show up in Files > On My iPad > GridCraft.
        if let Some(docs) = documents_dir() {
            let _ = std::fs::create_dir_all(docs.join("Fonts"));
            let _ = std::fs::create_dir_all(docs.join("Working"));
        }
        // Catalog the fonts on this iPad (the app starts this itself only in its desktop setup).
        gridcraft_ui_egui::fonts::start_scan();

        let gpu = Gpu::new(layer, width, height)?;
        let ctx = egui::Context::default();
        // Apple's shortcut set only: the app asks the context which system it runs on.
        ctx.set_os(egui::os::OperatingSystem::Mac);
        let deadline: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(Some(Instant::now())));
        {
            let dl = deadline.clone();
            ctx.set_request_repaint_callback(move |info| {
                let now = Instant::now();
                let at = now.checked_add(info.delay).unwrap_or(now + Duration::from_secs(3600));
                wake_at(&dl, at);
            });
        }
        let inbox: Inbox = Inbox::default();
        let pictures: Rc<RefCell<Option<Inbox>>> = Rc::new(RefCell::new(None));
        let pending_save: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let pending_bytes: Rc<RefCell<Option<Vec<u8>>>> = Rc::new(RefCell::new(None));
        let services = build_services(cb, &inbox, &pictures, &pending_save, &pending_bytes);

        let mut app = SheetApp::new(Session::new(), services);
        load_prefs(&mut app);
        if app.session.documents().is_empty() {
            app.session.new_workbook();
        }
        app.after_engine();
        SheetApp::setup_context_for_language(&ctx, app.ui.dark, app.ui.language);

        Ok(Host {
            app,
            ctx,
            gpu,
            scale,
            events: Vec::new(),
            modifiers: egui::Modifiers::NONE,
            dark: false,
            focused: true,
            start: Instant::now(),
            deadline,
            cb,
            inbox,
            pictures,
            pending_save,
            pending_bytes,
            wants_keyboard: false,
            zoom_pct: 100.0,
            fonts_logged: false,
        })
    }

    fn wake(&self) {
        wake_at(&self.deadline, Instant::now());
    }

    fn push(&mut self, e: egui::Event) {
        self.events.push(e);
        self.wake();
    }

    /// Tells egui which modifier keys (Shift, Command, ...) are down, whenever that changes.
    /// This egui version learns modifier state only from this event.
    fn set_modifiers(&mut self, m: egui::Modifiers) {
        if m != self.modifiers {
            self.modifiers = m;
            self.push(egui::Event::ModifiersChanged(m));
        }
    }

    /// Points in the UIKit sense, converted to egui points (the interface size preference zooms).
    fn pt(&self, x: f32, y: f32) -> egui::Pos2 {
        let z = self.ctx.zoom_factor().max(0.1);
        egui::pos2(x / z, y / z)
    }

    /// The sheet's zoom percentage (View > Zoom), 10 to 400.
    fn apply_zoom(&mut self, percent: f32) {
        let p = percent.clamp(10.0, 400.0);
        self.zoom_pct = p;
        let _ = self.app.run("view.zoom", serde_json::json!({ "percent": p.round() as i64 }));
        self.app.after_engine();
        self.wake();
    }

    fn handle_output(&self, out: &egui::PlatformOutput) {
        for cmd in &out.commands {
            match cmd {
                egui::OutputCommand::CopyText(text) => {
                    if let Some(f) = self.cb.copy_text {
                        let c = cstring(text);
                        unsafe { f(self.cb.user, c.as_ptr()) }
                    }
                }
                egui::OutputCommand::OpenUrl(open) => {
                    if let Some(f) = self.cb.open_url {
                        let c = cstring(&open.url);
                        unsafe { f(self.cb.user, c.as_ptr()) }
                    }
                }
                _ => {}
            }
        }
    }

    fn flags(&self, drew: bool) -> c_int {
        (drew as c_int) | ((self.wants_keyboard as c_int) << 1)
    }

    fn frame(&mut self) -> c_int {
        let now = Instant::now();
        let due = self.deadline.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|t| now >= t);
        if !due {
            return self.flags(false);
        }
        *self.deadline.lock().unwrap_or_else(|e| e.into_inner()) = None;

        if !self.fonts_logged && gridcraft_ui_egui::fonts::scan_done() {
            self.fonts_logged = true;
            let n = gridcraft_ui_egui::fonts::catalog().len();
            let fam = gridcraft_ui_egui::fonts::families().len();
            note(&format!("fonts: {n} faces cataloged, {fam} families in the font list"));
        }

        let ppp = self.scale * self.ctx.zoom_factor().max(0.1);
        let rect = egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(self.gpu.config.width as f32 / ppp, self.gpu.config.height as f32 / ppp),
        );
        let mut raw = egui::RawInput::default();
        raw.time = Some(self.start.elapsed().as_secs_f64());
        raw.screen_rect = Some(rect);
        raw.focused = self.focused;
        raw.system_theme = Some(if self.dark { egui::Theme::Dark } else { egui::Theme::Light });
        raw.max_texture_side = Some(self.gpu.device.limits().max_texture_dimension_2d as usize);
        raw.events = std::mem::take(&mut self.events);
        if let Some(v) = raw.viewports.get_mut(&egui::ViewportId::ROOT) {
            v.native_pixels_per_point = Some(self.scale);
            v.inner_rect = Some(rect);
        }

        let ctx = self.ctx.clone();
        self.app.raw_input_hook(&ctx, &mut raw);
        let logic = ctx.run_logic(&raw, |c| self.app.logic(c));
        let mut full = ctx.run_ui(raw, |ui| self.app.ui(ui));
        self.handle_output(&logic.platform_output);
        self.handle_output(&full.platform_output);
        self.wants_keyboard = ctx.egui_wants_keyboard_input();

        if let Some(vo) = full.viewport_output.get(&egui::ViewportId::ROOT)
            && let Some(at) = Instant::now().checked_add(vo.repaint_delay)
        {
            wake_at(&self.deadline, at);
        }

        let ppp_out = full.pixels_per_point;
        let shapes = std::mem::take(&mut full.shapes);
        let prims = ctx.tessellate(shapes, ppp_out);

        let deadline = self.deadline.clone();
        let gpu = &mut self.gpu;
        for (id, deltas) in full.textures_delta.set.drain() {
            for d in deltas {
                gpu.renderer.update_texture(&gpu.device, &gpu.queue, id, &d);
            }
        }

        if gpu.needs_reconfigure {
            gpu.surface.configure(&gpu.device, &gpu.config);
            gpu.needs_reconfigure = false;
        }
        let frame = if prims.is_empty() {
            // The app skips painting for one frame while it switches fonts. Showing that empty
            // frame would flash a blank screen, so the picture on screen is simply kept.
            None
        } else {
            match gpu.surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(f) => Some(f),
                wgpu::CurrentSurfaceTexture::Suboptimal(f) => {
                    gpu.needs_reconfigure = true;
                    Some(f)
                }
                _ => {
                    gpu.surface.configure(&gpu.device, &gpu.config);
                    wake_at(&deadline, Instant::now());
                    None
                }
            }
        };

        if let Some(frame) = frame {
            let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
            let mut encoder = gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("gridcraft") });
            let screen = egui_wgpu::ScreenDescriptor {
                size_in_pixels: [gpu.config.width, gpu.config.height],
                pixels_per_point: ppp_out,
            };
            let extra = gpu.renderer.update_buffers(&gpu.device, &gpu.queue, &mut encoder, &prims, &screen);
            {
                let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("gridcraft"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.11, g: 0.11, b: 0.12, a: 1.0 }),
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                gpu.renderer.render(&mut pass.forget_lifetime(), &prims, &screen);
            }
            gpu.queue.submit(extra.into_iter().chain(std::iter::once(encoder.finish())));
            gpu.queue.present(frame);
        }

        for id in full.textures_delta.free.drain() {
            gpu.renderer.free_texture(&id);
        }
        self.flags(true)
    }
}

// ---------------------------------------------------------------------------------------------
// The C interface Swift calls
// ---------------------------------------------------------------------------------------------

unsafe fn host<'a>(h: *mut c_void) -> Option<&'a mut Host> {
    unsafe { (h as *mut Host).as_mut() }
}

static HOOK: Once = Once::new();

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gc_create(layer: *mut c_void, width_px: u32, height_px: u32, scale: f32, cb: GCCallbacks) -> *mut c_void {
    HOOK.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            note(&format!("PANIC: {info}\n{}", std::backtrace::Backtrace::force_capture()));
        }));
    });
    guard(std::ptr::null_mut(), || match Host::new(layer, width_px, height_px, scale, cb) {
        Ok(h) => Box::into_raw(Box::new(h)) as *mut c_void,
        Err(e) => {
            note(&format!("start failed: {e}"));
            std::ptr::null_mut()
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gc_destroy(h: *mut c_void) {
    if !h.is_null() {
        guard((), || drop(unsafe { Box::from_raw(h as *mut Host) }));
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gc_resize(h: *mut c_void, width_px: u32, height_px: u32, scale: f32) {
    guard((), || {
        if let Some(h) = unsafe { host(h) } {
            h.scale = scale;
            h.gpu.config.width = width_px.max(1);
            h.gpu.config.height = height_px.max(1);
            h.gpu.surface.configure(&h.gpu.device, &h.gpu.config);
            h.wake();
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gc_set_dark(h: *mut c_void, dark: c_int) {
    if let Some(h) = unsafe { host(h) } {
        h.dark = dark != 0;
        h.wake();
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gc_set_focused(h: *mut c_void, focused: c_int) {
    if let Some(h) = unsafe { host(h) } {
        h.focused = focused != 0;
        h.wake();
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gc_pointer_moved(h: *mut c_void, x: f32, y: f32) {
    if let Some(h) = unsafe { host(h) } {
        let p = h.pt(x, y);
        h.push(egui::Event::PointerMoved(p));
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gc_pointer_gone(h: *mut c_void) {
    if let Some(h) = unsafe { host(h) } {
        h.push(egui::Event::PointerGone);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gc_pointer_button(h: *mut c_void, x: f32, y: f32, button: c_int, pressed: c_int, mods: u32) {
    if let Some(h) = unsafe { host(h) } {
        h.set_modifiers(to_mods(mods));
        let pos = h.pt(x, y);
        let button = match button {
            1 => egui::PointerButton::Secondary,
            2 => egui::PointerButton::Middle,
            _ => egui::PointerButton::Primary,
        };
        let m = h.modifiers;
        h.push(egui::Event::PointerButton { pos, button, pressed: pressed != 0, modifiers: m });
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gc_scroll(h: *mut c_void, dx: f32, dy: f32, mods: u32) {
    if let Some(h) = unsafe { host(h) } {
        h.set_modifiers(to_mods(mods));
        let z = h.ctx.zoom_factor().max(0.1);
        let m = h.modifiers;
        h.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(dx / z, dy / z),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
            modifiers: m,
        });
    }
}

/// Pinch: scales the sheet's zoom percentage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gc_zoom(h: *mut c_void, factor: f32) {
    if let Some(h) = unsafe { host(h) } {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let target = (h.zoom_pct * factor).clamp(10.0, 400.0);
        if (target.round() - h.zoom_pct.round()).abs() >= 1.0 {
            h.apply_zoom(target);
        } else {
            h.zoom_pct = target;
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gc_key(h: *mut c_void, hid: u32, pressed: c_int, repeat: c_int, mods: u32) {
    if let Some(h) = unsafe { host(h) } {
        // Zoom, as in Excel for Mac: Command+Option+= and Command+Option+-.
        if pressed != 0 && (mods & !1) == 12 && (hid == 0x2E || hid == 0x2D) {
            let step = if hid == 0x2E { 10.0 } else { -10.0 };
            let base = (h.zoom_pct / 10.0).round() * 10.0;
            h.apply_zoom(base + step);
            return;
        }
        h.set_modifiers(to_mods(mods));
        let Some(key) = hid_to_key(hid) else { return };
        let m = h.modifiers;
        h.push(egui::Event::Key { key, physical_key: Some(key), pressed: pressed != 0, repeat: repeat != 0, modifiers: m });
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gc_text(h: *mut c_void, utf8: *const c_char) {
    if let Some(h) = unsafe { host(h) }
        && let Some(s) = unsafe { from_c(utf8) }
        && !s.is_empty()
    {
        h.push(egui::Event::Text(s));
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gc_copy(h: *mut c_void) {
    if let Some(h) = unsafe { host(h) } {
        h.push(egui::Event::Copy);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gc_cut(h: *mut c_void) {
    if let Some(h) = unsafe { host(h) } {
        h.push(egui::Event::Cut);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gc_paste(h: *mut c_void, utf8: *const c_char) {
    if let Some(h) = unsafe { host(h) }
        && let Some(s) = unsafe { from_c(utf8) }
    {
        h.push(egui::Event::Paste(s));
    }
}

/// A file the person picked from Files: opened on the next frame like a dropped file.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gc_open_file(h: *mut c_void, name: *const c_char, data: *const u8, len: usize) {
    if let Some(h) = unsafe { host(h) }
        && let Some(name) = unsafe { from_c(name) }
    {
        note(&format!("a file arrived: {name}, {len} bytes"));
        let bytes = if data.is_null() || len == 0 { Vec::new() } else { unsafe { std::slice::from_raw_parts(data, len) }.to_vec() };
        h.inbox.lock().unwrap_or_else(|e| e.into_inner()).push((name, bytes));
        h.wake();
    }
}

/// A picture the person picked for the Insert Picture dialog.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gc_picture_file(h: *mut c_void, name: *const c_char, data: *const u8, len: usize) {
    if let Some(h) = unsafe { host(h) }
        && let Some(name) = unsafe { from_c(name) }
    {
        let bytes = if data.is_null() || len == 0 { Vec::new() } else { unsafe { std::slice::from_raw_parts(data, len) }.to_vec() };
        if let Some(target) = h.pictures.borrow_mut().take() {
            target.lock().unwrap_or_else(|e| e.into_inner()).push((name, bytes));
        }
        h.wake();
    }
}

/// The Save to Files sheet finished (ok = 1, with the name of the file the person chose) or was
/// cancelled (ok = 0). A saved workbook counts as saved, and from then on has a working copy in
/// the app's Working folder: Save and AutoSave write there silently and the shell copies the
/// result to the chosen file. Save As does not use that path; it always asks.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gc_save_result(h: *mut c_void, ok: c_int, final_name: *const c_char) {
    if let Some(h) = unsafe { host(h) } {
        let asked = h.pending_save.borrow_mut().take();
        let bytes = h.pending_bytes.borrow_mut().take();
        let chosen = unsafe { from_c(final_name) };
        if ok != 0 {
            let name = chosen.or(asked);
            if let Some(name) = name
                && name.to_ascii_lowercase().ends_with(".xlsx")
                && let Some(doc) = h.app.session.active_mut()
            {
                let working = documents_dir().map(|d| d.join("Working").join(name.replace('/', "_")));
                if let Some(path) = &working {
                    if let Some(parent) = path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    if let Some(bytes) = &bytes {
                        let _ = std::fs::write(path, bytes);
                    }
                }
                doc.saved = doc.wb.clone();
                if let Some(path) = working {
                    doc.path = Some(path.to_string_lossy().into_owned());
                }
                doc.title = name;
            }
        }
        h.wake();
    }
}

/// Called every display refresh. Bit 0: a frame was drawn. Bit 1: the on-screen keyboard is wanted.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gc_frame(h: *mut c_void) -> c_int {
    guard(0, || match unsafe { host(h) } {
        Some(h) => h.frame(),
        None => 0,
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gc_save_state(h: *mut c_void) {
    if let Some(h) = unsafe { host(h) } {
        save_prefs(&h.app);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gc_is_dirty(h: *mut c_void) -> c_int {
    match unsafe { host(h) } {
        Some(h) => h.app.session.documents().iter().any(|d| d.is_dirty()) as c_int,
        None => 0,
    }
}
