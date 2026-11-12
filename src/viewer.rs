//! The built-in viewer (Space): images, video/audio, PDF-less text/code/hex, Office text, archive listings,
//! HTML/Markdown. Behaviour and keys follow FileMaster. Loading happens on a worker thread; the UI thread only
//! draws and handles keys, so opening and scrolling never block.

use crate::gfx::{Align, Color, Face, Gfx, Rect};
use crate::theme::Theme;
use spacemaster::browser::{Key, KeyEvent};
use spacemaster::highlight::{self, Span, Tok};
use spacemaster::video::Player;
use spacemaster::web::WebView;
use spacemaster::{docs, format, paths};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{GENERIC_READ, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;
use windows::Win32::Graphics::DirectWrite::IDWriteTextLayout;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

pub const WM_VIEWER: u32 = 0x8000 + 3; // WM_APP + 3

const TITLE_H: f32 = 40.0;
const HINT_H: f32 = 28.0;
const SEEK_H: f32 = 52.0;
const TEXT_LIMIT: usize = 2_000_000;
const SYNTAX_LIMIT: usize = 300_000;
const MAX_LINE_CHARS: usize = 3000;

// MARK: Loading (worker thread)

pub enum Payload {
    Image { w: u32, h: u32, data: Vec<u8> },
    Text { text: String, spans: Vec<Span>, hex: bool },
    Html { html: String, file: Option<String> },
    Media,
    Pdf,
    PdfOpened { count: usize, first: (f32, f32) },
    PdfPage { page: usize, req_w: u32, w: u32, h: u32, data: Vec<u8> },
    Error(String),
}

pub struct Loaded {
    pub token: u32,
    pub payload: Payload,
    pub caption: Option<String>,
}

pub fn spawn_load(hwnd: isize, token: u32, path: String, dark: bool) {
    std::thread::Builder::new()
        .name("SpaceMaster.viewer-load".into())
        .spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            let (payload, caption) = load(&path, dark);
            let boxed = Box::new(Loaded { token, payload, caption });
            unsafe {
                if PostMessageW(Some(HWND(hwnd as *mut _)), WM_VIEWER, WPARAM(0), LPARAM(Box::into_raw(boxed) as isize)).is_err() {
                    // window gone: the box leaks with the process, nothing to do
                }
            }
        })
        .expect("spawn viewer loader");
}

fn load(path: &str, dark: bool) -> (Payload, Option<String>) {
    let name = paths::file_name(path);
    let kind = docs::kind_of(name);
    let size = std::fs::metadata(path).map(|m| m.len() as i64).unwrap_or(0);
    let size_text = format::bytes(size);
    match kind {
        docs::Kind::Image => {
            return match decode_image(path, 8192) {
                Ok((w, h, data)) => (Payload::Image { w, h, data }, Some(format!("{w} \u{d7} {h} \u{b7} {size_text}"))),
                Err(e) => (Payload::Error(format!("Can't decode this image ({e}).")), None),
            };
        }
        docs::Kind::Video | docs::Kind::Audio => return (Payload::Media, None),
        docs::Kind::Pdf => return (Payload::Pdf, None),
        docs::Kind::Office => {
            if let Some(d) = docs::extract_office(std::path::Path::new(path), 5000) {
                return (Payload::Text { text: d.text, spans: Vec::new(), hex: false }, Some(d.caption));
            }
        }
        docs::Kind::Archive => {
            if let Some(d) = docs::list_archive(std::path::Path::new(path), 20000) {
                return (Payload::Text { text: d.text, spans: Vec::new(), hex: false }, Some(d.caption));
            }
        }
        _ => {}
    }
    if matches!(kind, docs::Kind::Text | docs::Kind::Other) && docs::is_renderable(name) {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".html") || lower.ends_with(".htm") {
            return (Payload::Html { html: String::new(), file: Some(path.to_string()) }, Some(size_text));
        }
        if let Some(md) = docs::read_text_head(std::path::Path::new(path), TEXT_LIMIT) {
            return (Payload::Html { html: docs::markdown_to_html(&md, dark), file: None }, Some(size_text));
        }
    }
    if let Some(text) = docs::read_text_head(std::path::Path::new(path), TEXT_LIMIT) {
        let spans = match highlight::language_for(name) {
            Some(lang) if text.len() <= SYNTAX_LIMIT => highlight::tokenize(&text, lang),
            _ => Vec::new(),
        };
        return (Payload::Text { text, spans, hex: false }, Some(size_text));
    }
    // Binary: a real shell thumbnail if there is one, otherwise a hex dump like a hex editor.
    if let Some((w, h, data)) = crate::thumbs::fetch_image(path, 1024) {
        return (Payload::Image { w, h, data }, Some(size_text));
    }
    match docs::read_head_bytes(std::path::Path::new(path), 256 * 1024) {
        Some(bytes) => {
            let caption = format!("Hex view \u{b7} {} of {size_text}", format::bytes(bytes.len() as i64));
            (Payload::Text { text: docs::hex_dump(&bytes), spans: Vec::new(), hex: true }, Some(caption))
        }
        None => (Payload::Error("Can't read this file.".into()), None),
    }
}

/// Decodes any WIC-supported image to premultiplied BGRA; images larger than `max_side` are scaled down.
pub fn decode_image(path: &str, max_side: u32) -> Result<(u32, u32, Vec<u8>), String> {
    unsafe {
        let wic: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        let wide = paths::to_wide(path);
        let dec = wic
            .CreateDecoderFromFilename(PCWSTR(wide.as_ptr()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)
            .map_err(|e| e.to_string())?;
        let frame = dec.GetFrame(0).map_err(|e| e.to_string())?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).map_err(|e| e.to_string())?;
        let conv = wic.CreateFormatConverter().map_err(|e| e.to_string())?;
        conv.Initialize(&frame, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)
            .map_err(|e| e.to_string())?;
        let mut src: IWICBitmapSource = windows::core::Interface::cast(&conv).map_err(|e| e.to_string())?;
        if w.max(h) > max_side {
            let k = max_side as f32 / w.max(h) as f32;
            let (nw, nh) = (((w as f32 * k) as u32).max(1), ((h as f32 * k) as u32).max(1));
            let scaler = wic.CreateBitmapScaler().map_err(|e| e.to_string())?;
            scaler.Initialize(&src, nw, nh, WICBitmapInterpolationModeFant).map_err(|e| e.to_string())?;
            src = windows::core::Interface::cast(&scaler).map_err(|e| e.to_string())?;
            w = nw;
            h = nh;
        }
        let mut data = vec![0u8; (w as usize) * (h as usize) * 4];
        src.CopyPixels(std::ptr::null(), w * 4, &mut data).map_err(|e| e.to_string())?;
        Ok((w, h, data))
    }
}

// MARK: Views

struct ImageView {
    w: u32,
    h: u32,
    data: Vec<u8>,
    bmp: Option<(u32, ID2D1Bitmap)>,
    /// 1.0 = fitted to the window.
    zoom: f32,
    pan: (f32, f32),
    drag: Option<(f32, f32, (f32, f32))>,
}

struct TextView {
    text: String,
    starts: Vec<usize>,
    spans: Vec<Span>,
    hex: bool,
    scroll: f32,
    hscroll: f32,
    cache: HashMap<usize, IDWriteTextLayout>,
    cache_key: (u32, bool, u32),
    visible: usize,
}

struct PageBmp {
    req_w: u32,
    w: u32,
    h: u32,
    data: Vec<u8>,
    bmp: Option<(u32, ID2D1Bitmap)>,
}

struct PdfView {
    sizes: Vec<(f32, f32)>,
    scroll: f32,
    hscroll: f32,
    zoom: f32,
    pages: HashMap<usize, PageBmp>,
    requested: HashSet<(usize, u32)>,
    cur_page: usize,
    max_scroll: f32,
    view_h: f32,
}

enum PdfCmd {
    Open { token: u32, path: String },
    Render { token: u32, page: usize, width: u32 },
}

fn post_loaded(hwnd: isize, l: Loaded) {
    let boxed = Box::new(l);
    unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_VIEWER, WPARAM(0), LPARAM(Box::into_raw(boxed) as isize));
    }
}

/// Owns the PDF document (COM objects stay on this thread); renders requested pages, newest request first.
fn pdf_worker(hwnd: isize, rx: Receiver<PdfCmd>, current: Arc<AtomicU32>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let mut doc: Option<spacemaster::pdf::PdfDoc> = None;
    while let Ok(first) = rx.recv() {
        let mut batch = vec![first];
        while let Ok(c) = rx.try_recv() {
            batch.push(c);
        }
        for c in &batch {
            if let PdfCmd::Open { token, path } = c {
                if *token != current.load(Ordering::Relaxed) {
                    continue;
                }
                doc = None;
                match spacemaster::pdf::PdfDoc::open(path) {
                    Ok(d) => {
                        let count = d.page_count();
                        let first = d.page_size(0).unwrap_or((816.0, 1056.0));
                        let caption = format!("{count} page{}", if count == 1 { "" } else { "s" });
                        doc = Some(d);
                        post_loaded(hwnd, Loaded { token: *token, payload: Payload::PdfOpened { count, first }, caption: Some(caption) });
                    }
                    Err(e) => post_loaded(hwnd, Loaded { token: *token, payload: Payload::Error(e), caption: None }),
                }
            }
        }
        let mut seen = HashSet::new();
        for c in batch.iter().rev() {
            if let PdfCmd::Render { token, page, width } = c {
                if *token != current.load(Ordering::Relaxed) || !seen.insert((*page, *width)) {
                    continue;
                }
                if let Some(d) = &doc {
                    if let Ok((w, h, data)) = d.render(*page, *width) {
                        post_loaded(hwnd, Loaded { token: *token, payload: Payload::PdfPage { page: *page, req_w: *width, w, h, data }, caption: None });
                    }
                }
            }
        }
    }
}

enum Content {
    Loading,
    Pdf(PdfView),
    Image(ImageView),
    Text(TextView),
    Media,
    Web,
    Error(String),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Act {
    None,
    Close,
    Step(i32),
    Maximize,
    Fullscreen,
    Copy(String),
}

pub struct Viewer {
    pub active: bool,
    pub token: u32,
    pub path: String,
    pub name: String,
    caption: String,
    hints: String,
    content: Content,
    // engines (created on first use, kept for the session)
    player: Option<Player>,
    web: Option<WebView>,
    pdf_tx: Option<Sender<PdfCmd>>,
    pdf_token: Arc<AtomicU32>,
    web_zoom: f64,
    // media state (see FileMaster MainWindow.Video.cs)
    video_pending: bool,
    volume: i32,
    seek_target: i64,
    last_seek_applied: Option<Instant>,
    last_seek_tick: Option<Instant>,
    seek_streak: i32,
    scrubbing: bool,
    seek_hold: Option<(u16, f64, Instant)>,
    caption_hold_until: Option<Instant>,
    transient: Option<String>,
    bar_until: Instant,
    pub fullscreen: bool,
}

impl Viewer {
    pub fn new() -> Viewer {
        Viewer {
            active: false,
            token: 0,
            path: String::new(),
            name: String::new(),
            caption: String::new(),
            hints: String::new(),
            content: Content::Loading,
            player: None,
            web: None,
            pdf_tx: None,
            pdf_token: Arc::new(AtomicU32::new(0)),
            web_zoom: 1.0,
            video_pending: false,
            volume: 100,
            seek_target: 0,
            last_seek_applied: None,
            last_seek_tick: None,
            seek_streak: 0,
            scrubbing: false,
            seek_hold: None,
            caption_hold_until: None,
            transient: None,
            bar_until: Instant::now(),
            fullscreen: false,
        }
    }

    pub fn is_media(&self) -> bool {
        matches!(self.content, Content::Media)
    }

    /// Starts loading `path`; the result arrives as `WM_VIEWER` and is applied with [`Viewer::loaded`].
    pub fn open(&mut self, hwnd: isize, path: &str, dark: bool) {
        self.reset_content();
        self.token = self.token.wrapping_add(1);
        self.pdf_token.store(self.token, Ordering::Relaxed);
        self.active = true;
        self.path = path.to_string();
        self.name = paths::file_name(path).to_string();
        self.caption = "Loading\u{2026}".into();
        self.hints = String::new();
        self.content = Content::Loading;
        spawn_load(hwnd, self.token, path.to_string(), dark);
    }

    pub fn close(&mut self) {
        self.token = self.token.wrapping_add(1);
        self.pdf_token.store(self.token, Ordering::Relaxed);
        self.reset_content();
        self.active = false;
        self.fullscreen = false;
    }

    fn reset_content(&mut self) {
        if let Some(p) = &self.player {
            if p.volume() > 0 {
                self.volume = p.volume();
            }
            p.stop();
            p.show(false);
        }
        if let Some(w) = &self.web {
            w.show(false);
        }
        self.video_pending = false;
        self.scrubbing = false;
        self.seek_hold = None;
        self.transient = None;
        self.content = Content::Loading;
    }

    /// Applies a finished load (ignored when the user already moved on to another file).
    pub fn loaded(&mut self, hwnd: isize, l: Loaded, dark: bool, window_bg: Color) {
        if !self.active || l.token != self.token {
            return;
        }
        if let Some(c) = l.caption {
            self.caption = c;
        }
        match l.payload {
            Payload::Image { w, h, data } => {
                self.hints = "Esc/q close \u{b7} n/p next/prev file \u{b7} + \u{2212} 0 zoom \u{b7} h j k l / arrows pan \u{b7} f maximize".into();
                self.content = Content::Image(ImageView { w, h, data, bmp: None, zoom: 1.0, pan: (0.0, 0.0), drag: None });
            }
            Payload::Text { text, spans, hex } => {
                self.hints = if hex {
                    "Esc/q close \u{b7} j/k scroll \u{b7} PageUp/PageDown \u{b7} g/G top/bottom \u{b7} n/p next/prev file".into()
                } else {
                    "Esc/q close \u{b7} j/k scroll \u{b7} PageUp/PageDown \u{b7} g/G top/bottom \u{b7} Ctrl+C copy all \u{b7} n/p next/prev file".into()
                };
                let starts = line_starts(&text);
                self.content = Content::Text(TextView {
                    text,
                    starts,
                    spans,
                    hex,
                    scroll: 0.0,
                    hscroll: 0.0,
                    cache: HashMap::new(),
                    cache_key: (0, dark, 0),
                    visible: 30,
                });
            }
            Payload::Html { html, file } => {
                self.hints = "Esc/q close \u{b7} j/k scroll \u{b7} PageUp/PageDown \u{b7} g/G top/bottom \u{b7} + \u{2212} 0 zoom \u{b7} n/p next/prev file".into();
                if self.web.is_none() {
                    self.web = WebView::new(hwnd).ok();
                }
                match &self.web {
                    Some(w) => {
                        w.set_background(((window_bg.r * 255.0) as u32) << 16 | ((window_bg.g * 255.0) as u32) << 8 | (window_bg.b * 255.0) as u32);
                        self.web_zoom = 1.0;
                        w.set_zoom(1.0);
                        match file {
                            Some(f) => w.navigate_file(std::path::Path::new(&f), true),
                            None => w.show_html(&html, true),
                        }
                        w.show(true);
                        self.content = Content::Web;
                    }
                    None => self.content = Content::Error("The HTML renderer (WebView2) is not available on this PC.".into()),
                }
            }
            Payload::Media => self.start_media(hwnd),
            Payload::Pdf => {
                if self.pdf_tx.is_none() {
                    let (tx, rx) = channel();
                    let cur = self.pdf_token.clone();
                    std::thread::Builder::new().name("SpaceMaster.pdf".into()).spawn(move || pdf_worker(hwnd, rx, cur)).ok();
                    self.pdf_tx = Some(tx);
                }
                if let Some(tx) = &self.pdf_tx {
                    let _ = tx.send(PdfCmd::Open { token: self.token, path: self.path.clone() });
                }
            }
            Payload::PdfOpened { count, first } => {
                self.hints = "Esc/q close \u{b7} j/k scroll \u{b7} PageUp/PageDown \u{b7} g/G first/last \u{b7} + \u{2212} 0 zoom \u{b7} n/p next/prev file \u{b7} f maximize".into();
                self.content = Content::Pdf(PdfView {
                    sizes: vec![first; count.max(1)],
                    scroll: 0.0,
                    hscroll: 0.0,
                    zoom: 1.0,
                    pages: HashMap::new(),
                    requested: HashSet::new(),
                    cur_page: 0,
                    max_scroll: 0.0,
                    view_h: 600.0,
                });
            }
            Payload::PdfPage { page, req_w, w, h, data } => {
                if let Content::Pdf(pv) = &mut self.content {
                    pv.requested.remove(&(page, req_w));
                    if page < pv.sizes.len() && w > 0 {
                        pv.sizes[page] = (w as f32, h as f32);
                    }
                    pv.pages.insert(page, PageBmp { req_w, w, h, data, bmp: None });
                    // keep memory bounded: drop the pages farthest from the one being read
                    while pv.pages.len() > 12 {
                        let cur = pv.cur_page;
                        if let Some(far) = pv.pages.keys().copied().max_by_key(|p| (*p as i64 - cur as i64).abs()) {
                            pv.pages.remove(&far);
                        } else {
                            break;
                        }
                    }
                }
            }
            Payload::Error(e) => self.content = Content::Error(e),
        }
    }

    fn start_media(&mut self, hwnd: isize) {
        self.hints = "Esc/q close \u{b7} Space play/pause \u{b7} \u{2190}/\u{2192} seek 10s, hold to scrub (Shift 30s, Ctrl 1m) \u{b7} 0-9 jump to 0-90% \u{b7} j/k volume \u{b7} m mute \u{b7} f fullscreen \u{b7} n/p next/prev file".into();
        if self.player.is_none() {
            match Player::new(hwnd) {
                Ok(p) => self.player = Some(p),
                Err(e) => {
                    self.content = Content::Error(format!("The video engine is not available ({e})."));
                    return;
                }
            }
        }
        let Some(p) = self.player.as_mut() else { return };
        if let Err(e) = p.load(std::path::Path::new(&self.path)) {
            self.content = Content::Error(format!("Can't play this file ({e})."));
            return;
        }
        p.set_volume(self.volume);
        self.seek_streak = 0;
        self.seek_target = 0;
        self.video_pending = true;
        self.content = Content::Media;
    }

    // MARK: Painting

    fn content_rect(&self, g: &Gfx) -> Rect {
        let top = if self.fullscreen { 0.0 } else { TITLE_H };
        let mut bottom = if self.fullscreen { 0.0 } else { HINT_H };
        if self.is_media() && (!self.fullscreen || Instant::now() < self.bar_until) {
            bottom += SEEK_H;
        }
        Rect::new(0.0, top, g.width, (g.height - top - bottom).max(0.0))
    }

    /// Position/size of the native child windows (video, web view) in device pixels.
    pub fn place_children(&self, g: &Gfx) {
        let r = self.content_rect(g);
        let s = g.dpi / 96.0;
        let (x, y, w, h) = ((r.x * s) as i32, (r.y * s) as i32, (r.w * s) as i32, (r.h * s) as i32);
        if let (Content::Media, Some(p)) = (&self.content, &self.player) {
            p.set_rect(x, y, w, h);
        }
        if let (Content::Web, Some(web)) = (&self.content, &self.web) {
            web.set_rect(x, y, w, h);
        }
    }

    pub fn paint(&mut self, g: &mut Gfx, t: &Theme) {
        let (w, h) = (g.width, g.height);
        g.clear(t.window);
        let area = self.content_rect(g);
        if !self.fullscreen {
            g.fill(Rect::new(0.0, 0.0, w, TITLE_H), t.bar);
            g.text(&self.name, Face::Header, Rect::new(16.0, 0.0, w * 0.6, TITLE_H), Align::Left, t.text);
            let cap = self.current_caption();
            g.text(&cap, Face::Small, Rect::new(w * 0.45, 0.0, w * 0.55 - 16.0, TITLE_H), Align::Right, t.text2);
            g.hline(0.0, w, TITLE_H - 1.0, t.separator);
            g.fill(Rect::new(0.0, h - HINT_H, w, HINT_H), t.bar);
            g.hline(0.0, w, h - HINT_H, t.separator);
            g.text(&self.hints, Face::Small, Rect::new(16.0, h - HINT_H, w - 32.0, HINT_H), Align::Left, t.text3);
        }
        g.push_clip(area);
        let mut pdf_reqs: Vec<(usize, u32)> = Vec::new();
        match &mut self.content {
            Content::Loading => g.text("Loading\u{2026}", Face::Meta, Rect::new(area.x, area.y, area.w, area.h), Align::Center, t.text2),
            Content::Error(e) => {
                g.paragraph_centered(e, Face::Meta, Rect::new(area.x + 24.0, area.y + area.h / 2.0 - 20.0, area.w - 48.0, 80.0), t.text2);
            }
            Content::Image(im) => paint_image(g, t, area, im),
            Content::Text(tv) => paint_text(g, t, area, tv),
            Content::Pdf(pv) => pdf_reqs = paint_pdf(g, t, area, pv),
            Content::Media | Content::Web => {}
        }
        g.pop_clip();
        if let (Some(tx), false) = (&self.pdf_tx, pdf_reqs.is_empty()) {
            for (page, width) in pdf_reqs {
                let _ = tx.send(PdfCmd::Render { token: self.token, page, width });
            }
        }
        if self.is_media() {
            self.paint_seek_bar(g, t);
        }
    }

    fn current_caption(&self) -> String {
        if let Some(tr) = &self.transient {
            if self.caption_hold_until.is_some_and(|u| Instant::now() < u) {
                return tr.clone();
            }
        }
        if let Content::Pdf(pv) = &self.content {
            return format!("page {} of {}  \u{b7}  {}", pv.cur_page + 1, pv.sizes.len(), self.caption);
        }
        if let (Content::Media, Some(p)) = (&self.content, &self.player) {
            return format!("vol {}%{}{}", p.volume(), if p.muted() { " (muted)" } else { "" }, if p.is_playing() { "" } else { "   paused" });
        }
        self.caption.clone()
    }

    fn seek_rect(&self, g: &Gfx) -> Rect {
        let bottom = if self.fullscreen { g.height } else { g.height - HINT_H };
        Rect::new(0.0, bottom - SEEK_H, g.width, SEEK_H)
    }

    fn track_rect(&self, g: &Gfx) -> Rect {
        let r = self.seek_rect(g);
        Rect::new(r.x + 72.0, r.y + 22.0, (r.w - 144.0).max(20.0), 8.0)
    }

    fn shown_time(&self) -> (i64, i64) {
        let Some(p) = &self.player else { return (0, 0) };
        let len = p.length_ms().max(0);
        let recent = self.last_seek_applied.is_some_and(|t| t.elapsed() < Duration::from_millis(350));
        let time = if self.scrubbing || recent { self.seek_target } else { p.time_ms().max(0) };
        (time, len)
    }

    fn paint_seek_bar(&mut self, g: &mut Gfx, t: &Theme) {
        if self.fullscreen && Instant::now() >= self.bar_until {
            return;
        }
        let r = self.seek_rect(g);
        g.fill(r, t.bar);
        let tr = self.track_rect(g);
        let (time, len) = self.shown_time();
        let ratio = if len > 0 { (time as f32 / len as f32).clamp(0.0, 1.0) } else { 0.0 };
        g.fill_round(tr, 4.0, t.text.alpha(0.15));
        g.fill_round(Rect::new(tr.x, tr.y, (tr.w * ratio).max(0.0), tr.h), 4.0, t.accent);
        g.fill_round(Rect::new(tr.x + tr.w * ratio - 7.0, tr.y + tr.h / 2.0 - 7.0, 14.0, 14.0), 7.0, t.accent);
        g.text(&duration(time), Face::Small, Rect::new(r.x + 12.0, tr.y - 8.0, 56.0, 24.0), Align::Right, t.text2);
        g.text(&duration(len), Face::Small, Rect::new(tr.right() + 8.0, tr.y - 8.0, 60.0, 24.0), Align::Left, t.text2);
    }

    // MARK: Timer

    /// ~12 Hz; returns true when something visible changed.
    pub fn tick(&mut self) -> bool {
        let mut dirty = false;
        if self.is_media() {
            if let Some(p) = &self.player {
                if self.video_pending && p.frame_ready() {
                    self.video_pending = false;
                    if p.has_video() {
                        p.show(true);
                    }
                }
                dirty = true;
            }
            // our own key repeat while Left/Right is held (110 ms, no OS repeat delay)
            if let Some((vk, secs, last)) = self.seek_hold {
                if !key_is_down(vk as i32) {
                    self.seek_hold = None;
                } else if last.elapsed() >= Duration::from_millis(110) {
                    self.seek_hold = Some((vk, secs, Instant::now()));
                    self.seek_rel(secs);
                }
            }
        }
        dirty
    }

    pub fn needs_timer(&self) -> bool {
        self.active && self.is_media()
    }

    // MARK: Keys

    pub fn key(&mut self, ev: KeyEvent, vk: u16, repeat: bool) -> Act {
        let media = self.is_media();
        let a = match ev.key {
            Key::Escape => return Act::Close,
            Key::Char(' ') => {
                if media {
                    self.toggle_pause();
                    return Act::None;
                }
                return Act::Close;
            }
            Key::Left if media => return self.media_seek_key(ev, vk, repeat, -1.0),
            Key::Right if media => return self.media_seek_key(ev, vk, repeat, 1.0),
            Key::Up if media => {
                self.adjust_volume(5);
                return Act::None;
            }
            Key::Down if media => {
                self.adjust_volume(-5);
                return Act::None;
            }
            Key::Up => Nav::Up,
            Key::Down => Nav::Down,
            Key::Left => Nav::Left,
            Key::Right => Nav::Right,
            Key::PageUp => Nav::PageUp,
            Key::PageDown => Nav::PageDown,
            Key::Home => Nav::Top,
            Key::End => Nav::Bottom,
            Key::Char('c') if ev.ctrl => {
                if let Content::Text(tv) = &self.content {
                    return Act::Copy(tv.text.clone());
                }
                return Act::None;
            }
            _ => return Act::None,
        };
        self.navigate(a)
    }

    pub fn ch(&mut self, c: char, repeat: bool) -> Act {
        let media = self.is_media();
        let vk_for_hold = c.to_ascii_uppercase() as u16;
        if media && c.is_ascii_digit() {
            self.seek_percent((c as u8 - b'0') as i64 * 10);
            return Act::None;
        }
        let nav = match c {
            ' ' => {
                if media {
                    self.toggle_pause();
                    return Act::None;
                }
                return Act::Close;
            }
            'q' => return Act::Close,
            'n' => return Act::Step(1),
            'p' => return Act::Step(-1),
            'h' if media => return self.media_seek_key(KeyEvent::ch('h'), vk_for_hold, repeat, -1.0),
            'l' if media => return self.media_seek_key(KeyEvent::ch('l'), vk_for_hold, repeat, 1.0),
            'k' if media => {
                self.adjust_volume(5);
                return Act::None;
            }
            'j' if media => {
                self.adjust_volume(-5);
                return Act::None;
            }
            'h' => Nav::Left,
            'l' => Nav::Right,
            'k' => Nav::Up,
            'j' => Nav::Down,
            '+' | '=' => Nav::ZoomIn,
            '-' => Nav::ZoomOut,
            '0' => Nav::ZoomReset,
            'f' => return if media { Act::Fullscreen } else { Act::Maximize },
            'm' if media => {
                self.toggle_mute();
                return Act::None;
            }
            'g' => Nav::Top,
            'G' => Nav::Bottom,
            _ => return Act::None,
        };
        self.navigate(nav)
    }

    /// Ctrl+Plus/Minus/0 (read from key-down by the window procedure).
    pub fn zoom_key(&mut self, dir: i8) {
        let n = match dir {
            1 => Nav::ZoomIn,
            -1 => Nav::ZoomOut,
            _ => Nav::ZoomReset,
        };
        self.navigate(n);
    }

    fn navigate(&mut self, n: Nav) -> Act {
        match &mut self.content {
            Content::Image(im) => match n {
                Nav::Up => im.pan.1 += 96.0,
                Nav::Down => im.pan.1 -= 96.0,
                Nav::Left => im.pan.0 += 96.0,
                Nav::Right => im.pan.0 -= 96.0,
                Nav::PageUp => im.pan.1 += 600.0,
                Nav::PageDown => im.pan.1 -= 600.0,
                Nav::Top => im.pan.1 = 1e6,
                Nav::Bottom => im.pan.1 = -1e6,
                Nav::ZoomIn => im.zoom = (im.zoom * 1.25).min(60.0),
                Nav::ZoomOut => im.zoom = (im.zoom / 1.25).max(0.05),
                Nav::ZoomReset => {
                    im.zoom = 1.0;
                    im.pan = (0.0, 0.0);
                }
            },
            Content::Text(tv) => {
                let page = (tv.visible as f32 * 0.9).max(1.0);
                let max = tv.max_scroll();
                match n {
                    Nav::Up => tv.scroll -= 3.0,
                    Nav::Down => tv.scroll += 3.0,
                    Nav::Left => tv.hscroll -= 96.0,
                    Nav::Right => tv.hscroll += 96.0,
                    Nav::PageUp => tv.scroll -= page,
                    Nav::PageDown => tv.scroll += page,
                    Nav::Top => {
                        tv.scroll = 0.0;
                        tv.hscroll = 0.0;
                    }
                    Nav::Bottom => tv.scroll = max,
                    _ => {}
                }
                tv.scroll = tv.scroll.clamp(0.0, max);
                tv.hscroll = tv.hscroll.max(0.0);
            }
            Content::Pdf(pv) => {
                let page = (pv.view_h * 0.9).max(1.0);
                match n {
                    Nav::Up => pv.scroll -= 96.0,
                    Nav::Down => pv.scroll += 96.0,
                    Nav::Left => pv.hscroll -= 96.0,
                    Nav::Right => pv.hscroll += 96.0,
                    Nav::PageUp => pv.scroll -= page,
                    Nav::PageDown => pv.scroll += page,
                    Nav::Top => {
                        pv.scroll = 0.0;
                        pv.hscroll = 0.0;
                    }
                    Nav::Bottom => pv.scroll = pv.max_scroll,
                    Nav::ZoomIn => pv.zoom = (pv.zoom * 1.25).min(5.0),
                    Nav::ZoomOut => pv.zoom = (pv.zoom / 1.25).max(0.3),
                    Nav::ZoomReset => {
                        pv.zoom = 1.0;
                        pv.hscroll = 0.0;
                    }
                }
                pv.scroll = pv.scroll.clamp(0.0, pv.max_scroll);
            }
            Content::Web => {
                if let Some(w) = &self.web {
                    match n {
                        Nav::Up => w.scroll_by(0.0, -96.0),
                        Nav::Down => w.scroll_by(0.0, 96.0),
                        Nav::Left => w.scroll_by(-96.0, 0.0),
                        Nav::Right => w.scroll_by(96.0, 0.0),
                        Nav::PageUp => w.page_up(),
                        Nav::PageDown => w.page_down(),
                        Nav::Top => w.scroll_to_top(),
                        Nav::Bottom => w.scroll_to_bottom(),
                        Nav::ZoomIn => {
                            self.web_zoom = (self.web_zoom * 1.25).min(5.0);
                            w.set_zoom(self.web_zoom);
                        }
                        Nav::ZoomOut => {
                            self.web_zoom = (self.web_zoom / 1.25).max(0.25);
                            w.set_zoom(self.web_zoom);
                        }
                        Nav::ZoomReset => {
                            self.web_zoom = 1.0;
                            w.set_zoom(1.0);
                        }
                    }
                }
            }
            _ => {}
        }
        Act::None
    }

    // MARK: Mouse

    pub fn mouse_down(&mut self, g: &Gfx, x: f32, y: f32) -> bool {
        if self.is_media() {
            let r = self.seek_rect(g);
            if r.contains(x, y) {
                self.scrubbing = true;
                self.seek_to_pointer(g, x);
                return true;
            }
        }
        if let Content::Image(im) = &mut self.content {
            im.drag = Some((x, y, im.pan));
            return true;
        }
        false
    }

    pub fn mouse_move(&mut self, g: &Gfx, x: f32, y: f32) -> bool {
        if self.scrubbing {
            self.seek_to_pointer(g, x);
            return true;
        }
        if self.fullscreen && self.is_media() {
            self.bar_until = Instant::now() + Duration::from_millis(1100);
        }
        if let Content::Image(im) = &mut self.content {
            if let Some((sx, sy, p0)) = im.drag {
                im.pan = (p0.0 + x - sx, p0.1 + y - sy);
                return true;
            }
        }
        false
    }

    pub fn mouse_up(&mut self) {
        self.scrubbing = false;
        if let Content::Image(im) = &mut self.content {
            im.drag = None;
        }
    }

    pub fn wheel(&mut self, delta: i32, ctrl: bool) {
        let notch = delta as f32 / 120.0;
        match &mut self.content {
            Content::Image(im) => {
                if ctrl {
                    im.zoom = (im.zoom * 1.15f32.powf(notch)).clamp(0.05, 60.0);
                } else {
                    im.pan.1 += notch * 96.0;
                }
            }
            Content::Text(tv) => {
                tv.scroll = (tv.scroll - notch * 3.0).clamp(0.0, tv.max_scroll());
            }
            Content::Pdf(pv) => {
                if ctrl {
                    pv.zoom = (pv.zoom * 1.15f32.powf(notch)).clamp(0.3, 5.0);
                } else {
                    pv.scroll = (pv.scroll - notch * 96.0 * 3.0).clamp(0.0, pv.max_scroll);
                }
            }
            Content::Media => self.adjust_volume((notch * 5.0) as i32),
            _ => {}
        }
    }

    // MARK: Media actions (FileMaster semantics)

    fn flash_bar(&mut self) {
        self.bar_until = Instant::now() + Duration::from_millis(1100);
    }

    fn transient(&mut self, text: String) {
        self.transient = Some(text);
        self.caption_hold_until = Some(Instant::now() + Duration::from_millis(1200));
    }

    fn step_seconds(ev: &KeyEvent) -> f64 {
        match (ev.ctrl, ev.shift) {
            (true, true) => 300.0,
            (true, false) => 60.0,
            (false, true) => 30.0,
            _ => 10.0,
        }
    }

    fn media_seek_key(&mut self, ev: KeyEvent, vk: u16, repeat: bool, dir: f64) -> Act {
        // the OS key repeat is ignored: our own 110 ms repeat (see tick) scrubs while the key is held
        if repeat {
            return Act::None;
        }
        let secs = dir * Self::step_seconds(&ev);
        self.seek_rel(secs);
        self.seek_hold = Some((vk, secs, Instant::now()));
        Act::None
    }

    /// Relative seek. Rapid repeats accelerate (up to 8x) and chain from the last target, not the stale clock.
    fn seek_rel(&mut self, mut seconds: f64) {
        let Some(p) = &self.player else { return };
        let len = p.length_ms();
        if len <= 0 {
            return;
        }
        let now = Instant::now();
        self.seek_streak = if self.last_seek_tick.is_some_and(|t| now.duration_since(t) < Duration::from_millis(450)) { self.seek_streak + 1 } else { 0 };
        self.last_seek_tick = Some(now);
        seconds *= (1 + self.seek_streak / 3).min(8) as f64;
        let from = if self.last_seek_applied.is_some_and(|t| now.duration_since(t) < Duration::from_millis(400)) { self.seek_target } else { p.time_ms() };
        self.seek_target = (from + (seconds * 1000.0) as i64).clamp(0, (len - 300).max(0));
        p.seek_to_ms(self.seek_target);
        self.last_seek_applied = Some(now);
        self.flash_bar();
    }

    fn seek_percent(&mut self, percent: i64) {
        let Some(p) = &self.player else { return };
        let len = p.length_ms();
        if len <= 0 {
            return;
        }
        self.seek_target = (len * percent / 100).clamp(0, (len - 300).max(0));
        self.last_seek_applied = Some(Instant::now());
        self.seek_streak = 0;
        p.seek_to_ms(self.seek_target);
        if self.fullscreen {
            self.flash_bar();
        } else {
            self.transient(format!("{percent}%"));
        }
    }

    fn seek_to_pointer(&mut self, g: &Gfx, x: f32) {
        let Some(p) = &self.player else { return };
        let len = p.length_ms();
        if len <= 0 {
            return;
        }
        let tr = self.track_rect(g);
        let ratio = ((x - tr.x) / tr.w.max(1.0)).clamp(0.0, 1.0) as f64;
        self.seek_target = (ratio * len as f64) as i64;
        self.last_seek_applied = Some(Instant::now());
        p.seek_to_ms(self.seek_target);
        self.flash_bar();
    }

    fn toggle_pause(&mut self) {
        if let Some(p) = &self.player {
            p.toggle_pause();
        }
        self.flash_bar();
    }

    fn adjust_volume(&mut self, delta: i32) {
        let Some(p) = &self.player else { return };
        let v = (p.volume() + delta).clamp(0, 150);
        p.set_volume(v);
        self.volume = v;
        self.transient(format!("Volume {v}%"));
    }

    fn toggle_mute(&mut self) {
        let Some(p) = &self.player else { return };
        p.set_muted(!p.muted());
        let text = if p.muted() { "Muted".to_string() } else { format!("Volume {}%", p.volume()) };
        self.transient(text);
    }
}

#[derive(Clone, Copy)]
enum Nav {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Top,
    Bottom,
    ZoomIn,
    ZoomOut,
    ZoomReset,
}

fn key_is_down(vk: i32) -> bool {
    unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(vk) < 0 }
}

fn duration(ms: i64) -> String {
    let s = (ms.max(0) / 1000) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

// MARK: Image

fn paint_image(g: &mut Gfx, t: &Theme, area: Rect, im: &mut ImageView) {
    let gen = g.generation();
    if im.bmp.as_ref().map(|b| b.0 != gen).unwrap_or(true) {
        im.bmp = g.create_bitmap_bgra(im.w, im.h, &im.data).map(|b| (gen, b));
    }
    let Some((_, bmp)) = &im.bmp else {
        g.text("Can't show this image.", Face::Meta, area, Align::Center, t.text2);
        return;
    };
    let fit = ((area.w - 8.0) / im.w as f32).min((area.h - 8.0) / im.h as f32).max(0.001);
    let scale = fit * im.zoom;
    let (dw, dh) = (im.w as f32 * scale, im.h as f32 * scale);
    // Smaller than the window: centred. Larger: pannable until the edge reaches the window edge.
    let max_x = ((dw - area.w) / 2.0).max(0.0);
    let max_y = ((dh - area.h) / 2.0).max(0.0);
    im.pan.0 = if dw <= area.w { 0.0 } else { im.pan.0.clamp(-max_x - 8.0, max_x + 8.0) };
    im.pan.1 = if dh <= area.h { 0.0 } else { im.pan.1.clamp(-max_y - 8.0, max_y + 8.0) };
    let dest = Rect::new(g.snap(area.x + (area.w - dw) / 2.0 + im.pan.0), g.snap(area.y + (area.h - dh) / 2.0 + im.pan.1), dw, dh);
    g.draw_scaled(bmp, dest);
}

// MARK: Text

fn line_starts(text: &str) -> Vec<usize> {
    let mut v = vec![0usize];
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' && i + 1 < text.len() {
            v.push(i + 1);
        }
    }
    v
}

impl TextView {
    fn max_scroll(&self) -> f32 {
        (self.starts.len() as f32 - self.visible as f32 + 1.0).max(0.0)
    }

    fn line(&self, i: usize) -> (usize, usize) {
        let s = self.starts[i];
        let mut e = self.starts.get(i + 1).copied().unwrap_or(self.text.len());
        let b = self.text.as_bytes();
        while e > s && (b[e - 1] == b'\n' || b[e - 1] == b'\r') {
            e -= 1;
        }
        (s, e)
    }

}

fn paint_text(g: &mut Gfx, t: &Theme, area: Rect, tv: &mut TextView) {
    let size: f32 = if tv.hex { 14.0 } else { 15.0 };
    let line_h = (size * 1.4).round();
    tv.visible = ((area.h / line_h).floor() as usize).max(1);
    tv.scroll = tv.scroll.clamp(0.0, tv.max_scroll());
    let gen = g.generation();
    let key = (gen, t.dark, (size * 10.0) as u32);
    if tv.cache_key != key {
        tv.cache.clear();
        tv.cache_key = key;
    }
    if tv.cache.len() > 400 {
        tv.cache.clear();
    }
    let first = tv.scroll.floor() as usize;
    let last = (first + tv.visible + 1).min(tv.starts.len());
    let default = Color::rgb(highlight::color(Tok::Plain, t.dark));
    let x0 = area.x + 20.0 - tv.hscroll;
    for i in first..last {
        let y = area.y + 8.0 + (i - first) as f32 * line_h;
        if !tv.cache.contains_key(&i) {
            let (s, e) = tv.line(i);
            let mut line = &tv.text[s..e];
            let mut cut = e;
            if line.chars().count() > MAX_LINE_CHARS {
                let end = line.char_indices().nth(MAX_LINE_CHARS).map(|(b, _)| b).unwrap_or(line.len());
                line = &line[..end];
                cut = s + end;
            }
            let spans = spans_for_line(&tv.spans, &tv.text, s, cut, t.dark);
            if let Some(l) = g.code_layout(line, size, &spans) {
                tv.cache.insert(i, l);
            }
        }
        if let Some(l) = tv.cache.get(&i) {
            g.draw_layout(l, x0, y, default);
        }
    }
    // scroll thumb
    let n = tv.starts.len();
    if n > tv.visible {
        let h = area.h;
        let thumb = (h * tv.visible as f32 / n as f32).max(24.0);
        let ty = (h - thumb) * (tv.scroll / tv.max_scroll().max(1.0)).min(1.0);
        g.fill_round(Rect::new(area.right() - 8.0, area.y + ty + 2.0, 4.0, thumb - 4.0), 2.0, t.text.alpha(0.25));
    }
}

/// Spans overlapping `[s, e)` as UTF-16 ranges relative to the line start.
fn spans_for_line(spans: &[Span], text: &str, s: usize, e: usize, dark: bool) -> Vec<(u32, u32, u32)> {
    if spans.is_empty() {
        return Vec::new();
    }
    let first = spans.partition_point(|sp| sp.start + sp.len <= s);
    let line = &text[s..e];
    let ascii = line.is_ascii();
    let u16_off = |byte: usize| -> u32 {
        if ascii { byte as u32 } else { line[..byte].encode_utf16().count() as u32 }
    };
    let mut out = Vec::new();
    for sp in &spans[first..] {
        if sp.start >= e {
            break;
        }
        if sp.kind == Tok::Plain {
            continue;
        }
        let a = sp.start.max(s) - s;
        let b = (sp.start + sp.len).min(e) - s;
        if b > a {
            let (ua, ub) = (u16_off(a), u16_off(b));
            out.push((ua, ub - ua, highlight::color(sp.kind, dark)));
        }
    }
    out
}

impl Viewer {
    /// What is currently shown ("loading", "image", "text", "hex", "media", "web", "error"); used by the self-test.
    pub fn state_name(&self) -> &'static str {
        match &self.content {
            Content::Loading => "loading",
            Content::Image(_) => "image",
            Content::Pdf(_) => "pdf",
            Content::Text(t) if t.hex => "hex",
            Content::Text(_) => "text",
            Content::Media => "media",
            Content::Web => "web",
            Content::Error(_) => "error",
        }
    }

    /// (first visible line, total lines) of a text view.
    pub fn text_position(&self) -> Option<(usize, usize)> {
        match &self.content {
            Content::Text(t) => Some((t.scroll.floor() as usize, t.starts.len())),
            _ => None,
        }
    }
}

// MARK: PDF

/// Draws the visible pages of a continuous view; returns the (page, pixel width) renders that are still needed.
fn paint_pdf(g: &mut Gfx, t: &Theme, area: Rect, pv: &mut PdfView) -> Vec<(usize, u32)> {
    let s = g.dpi / 96.0;
    let gap = 12.0;
    let pw = ((area.w - 24.0).min(1100.0) * pv.zoom).max(60.0);
    let n = pv.sizes.len();
    let mut tops = Vec::with_capacity(n);
    let mut y = gap;
    for &(w, h) in &pv.sizes {
        tops.push(y);
        y += pw * h / w.max(1.0) + gap;
    }
    pv.view_h = area.h;
    pv.max_scroll = (y - area.h).max(0.0);
    pv.scroll = pv.scroll.clamp(0.0, pv.max_scroll);
    let max_h = ((pw - area.w) / 2.0 + 12.0).max(0.0);
    pv.hscroll = pv.hscroll.clamp(-max_h, max_h);
    let need_w = (((pw * s) / 8.0).ceil() * 8.0).max(16.0) as u32;
    let mut reqs: Vec<(usize, u32)> = Vec::new();
    let first = tops.partition_point(|&tp| tp <= pv.scroll).saturating_sub(1);
    let mid = pv.scroll + area.h / 2.0;
    let gen = g.generation();
    for i in first..n {
        let top = tops[i] - pv.scroll + area.y;
        if top > area.bottom() {
            break;
        }
        let ph = pw * pv.sizes[i].1 / pv.sizes[i].0.max(1.0);
        if tops[i] <= mid && mid <= tops[i] + ph + gap {
            pv.cur_page = i;
        }
        let x = area.x + (area.w - pw) / 2.0 - pv.hscroll;
        let dest = Rect::new(g.snap(x), g.snap(top), pw, ph);
        g.fill_round(dest.inset(-1.0), 2.0, t.separator);
        g.fill(dest, Color::rgb(0xFFFFFF));
        let mut have = false;
        if let Some(pb) = pv.pages.get_mut(&i) {
            if pb.bmp.as_ref().map(|b| b.0 != gen).unwrap_or(true) {
                pb.bmp = g.create_bitmap_bgra(pb.w, pb.h, &pb.data).map(|b| (gen, b));
            }
            if let Some((_, bmp)) = &pb.bmp {
                g.draw_scaled(bmp, dest);
                have = true;
            }
            if pb.req_w != need_w && !pv.requested.contains(&(i, need_w)) {
                reqs.push((i, need_w));
                pv.requested.insert((i, need_w));
            }
        }
        if !have && !pv.pages.contains_key(&i) && !pv.requested.contains(&(i, need_w)) {
            reqs.push((i, need_w));
            pv.requested.insert((i, need_w));
        }
    }
    // read ahead: the page after the last visible one
    let next = (first..n).take_while(|&i| tops[i] - pv.scroll + area.y <= area.bottom()).last().map(|i| i + 1).unwrap_or(first);
    if next < n && !pv.pages.contains_key(&next) && !pv.requested.contains(&(next, need_w)) {
        reqs.push((next, need_w));
        pv.requested.insert((next, need_w));
    }
    // scroll thumb
    if pv.max_scroll > 0.0 {
        let h = area.h;
        let thumb = (h * area.h / (area.h + pv.max_scroll)).max(24.0);
        let ty = (h - thumb) * (pv.scroll / pv.max_scroll);
        g.fill_round(Rect::new(area.right() - 8.0, area.y + ty + 2.0, 4.0, thumb - 4.0), 2.0, t.text.alpha(0.25));
    }
    reqs
}
impl Viewer {
    /// (pages, pages rendered so far, first visible page) of a PDF view; used by the self-test.
    pub fn pdf_info(&self) -> Option<(usize, usize, usize)> {
        match &self.content {
            Content::Pdf(p) => Some((p.sizes.len(), p.pages.values().filter(|b| b.bmp.is_some()).count(), p.cur_page)),
            _ => None,
        }
    }
}