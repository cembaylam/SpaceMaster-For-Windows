//! The main window: layout, painting and input. All drawing is custom (Direct2D); only visible rows are drawn.

use crate::gfx::{Align, Color, Face, Gfx, Rect};
use crate::shell;
use crate::theme::Theme;
use spacemaster::browser::{run_delete, Browser, DeleteOutcome, DeleteRequest, Event, Key, KeyEvent, PromptKind};
use spacemaster::format;
use spacemaster::listing::Kind;
use spacemaster::model::{share_bucket, share_hue, DirState, SortKey};
use spacemaster::paths;
use spacemaster::safety::Level;
use spacemaster::settings::Settings;
use std::collections::HashSet;
use std::sync::atomic::{AtomicPtr, Ordering};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, ScreenToClient, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

mod selftest;
use crate::thumbs;
use crate::viewer::{self, Act};
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;

struct Thumb {
    w: u32,
    h: u32,
    data: Vec<u8>,
    bmp: Option<(u32, ID2D1Bitmap)>,
}

const SIDEBAR_W: f32 = 212.0;
const TOP_H: f32 = 44.0;
const HEADER_H: f32 = 30.0;
const STATUS_H: f32 = 28.0;
const BANNER_H: f32 = 32.0;
const ROW_H: f32 = 24.0;
const WM_DELETED: u32 = WM_APP + 1;
const TIMER_ID: usize = 1;

static APP_PTR: AtomicPtr<App> = AtomicPtr::new(std::ptr::null_mut());

fn app() -> Option<&'static mut App> {
    let p = APP_PTR.load(Ordering::Relaxed);
    if p.is_null() { None } else { Some(unsafe { &mut *p }) }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hit {
    Back,
    Forward,
    Up,
    Filter,
    Path(usize),
    PathBox,
    Sort,
    Hidden,
    Rescan,
    Help,
    Side(usize),
    BannerAction,
    BannerClose,
}

struct SideItem {
    title: String,
    detail: String,
    path: String,
    glyph: &'static str,
    header: bool,
    fraction: f32,
    rect: Rect,
}

#[derive(Default, Clone, Copy)]
struct ColView {
    offset: usize,
}

struct Layout {
    sidebar: Rect,
    top: Rect,
    headers: [Rect; 3],
    lists: [Rect; 3],
    banner: Option<Rect>,
    status: Rect,
}

pub struct App {
    hwnd: HWND,
    gfx: Gfx,
    theme: Theme,
    browser: Browser,
    cols: [ColView; 3],
    hover: Option<Hit>,
    hover_row: Option<(usize, usize)>,
    mouse: (f32, f32),
    hits: Vec<(Rect, Hit)>,
    side: Vec<SideItem>,
    path_segs: Vec<String>,
    last_cwd: String,
    last_cursor: Option<String>,
    last_preview: Option<String>,
    spinner: f32,
    help: bool,
    banner_dismissed: bool,
    elevated: bool,
    viewer: viewer::Viewer,
    fs_saved: Option<(isize, WINDOWPLACEMENT)>,
    timer_on: bool,
    tracking: bool,
    wheel: [f32; 3],
    high_surrogate: Option<u16>,
    thumbs: std::collections::HashMap<String, Thumb>,
    thumb_order: Vec<String>,
    thumb_requested: Option<String>,
    thumb_worker: thumbs::Worker,
}

impl App {
    fn layout(&self) -> Layout {
        let (w, h) = (self.gfx.width, self.gfx.height);
        let banner = self.banner_visible();
        let status = Rect::new(SIDEBAR_W, h - STATUS_H, w - SIDEBAR_W, STATUS_H);
        let bottom = status.y - if banner { BANNER_H } else { 0.0 };
        let main_w = w - SIDEBAR_W;
        let (w0, w1) = ((main_w * 0.27).floor(), (main_w * 0.43).floor());
        let xs = [SIDEBAR_W, SIDEBAR_W + w0, SIDEBAR_W + w0 + w1];
        let ws = [w0, w1, main_w - w0 - w1];
        let y = TOP_H;
        let mut headers = [Rect::new(0.0, 0.0, 0.0, 0.0); 3];
        let mut lists = headers;
        for i in 0..3 {
            headers[i] = Rect::new(xs[i], y, ws[i], HEADER_H);
            lists[i] = Rect::new(xs[i], y + HEADER_H, ws[i], (bottom - y - HEADER_H).max(0.0));
        }
        Layout {
            sidebar: Rect::new(0.0, 0.0, SIDEBAR_W, h),
            top: Rect::new(SIDEBAR_W, 0.0, main_w, TOP_H),
            headers,
            lists,
            banner: banner.then(|| Rect::new(SIDEBAR_W, bottom, main_w, BANNER_H)),
            status,
        }
    }

    fn banner_visible(&self) -> bool {
        let sc = &self.browser.model.scanner;
        !self.elevated && !self.banner_dismissed && !sc.is_scanning() && sc.denied() > 0
    }

    fn visible_rows(&self, list: Rect) -> usize {
        ((list.h / ROW_H).floor() as usize).max(1)
    }

    // MARK: Scroll bookkeeping

    /// Asks the worker for a thumbnail of the file under the cursor (once per file version).
    fn request_thumb(&mut self) {
        if self.browser.preview.is_some() {
            return;
        }
        let Some(row) = self.browser.cursor_row() else { return };
        if row.is_dir() {
            return;
        }
        let path = self.browser.path_of(&row.name);
        let key = format!("{path}|{}|{}", row.mtime, row.size);
        if self.thumbs.contains_key(&key) || self.thumb_requested.as_deref() == Some(key.as_str()) {
            return;
        }
        let lay = self.layout();
        let s = self.gfx.dpi / 96.0;
        let w = ((lay.lists[2].w - 40.0).max(64.0) * s).min(1024.0) as u32;
        let h = (lay.lists[2].h.min(420.0).max(64.0) * s) as u32;
        self.thumb_requested = Some(key.clone());
        self.thumb_worker.request(thumbs::Request { key, path, w, h });
    }

    fn thumb_arrived(&mut self, r: thumbs::Result) {
        if self.thumb_requested.as_deref() == Some(r.key.as_str()) {
            self.thumb_requested = None;
        }
        self.thumb_order.retain(|k| k != &r.key);
        self.thumb_order.push(r.key.clone());
        if self.thumb_order.len() > 40 {
            let old = self.thumb_order.remove(0);
            self.thumbs.remove(&old);
        }
        self.thumbs.insert(r.key, Thumb { w: r.w, h: r.h, data: r.data, bmp: None });
        self.invalidate();
    }

    fn sync_view(&mut self) {
        let lay = self.layout();
        let vis = self.visible_rows(lay.lists[1]);
        self.browser.visible_rows = vis;
        let cwd = self.browser.cwd.clone();
        let cursor = self.browser.cursor_name.clone();
        let cwd_changed = cwd != self.last_cwd;
        let cursor_changed = cursor != self.last_cursor;
        // Current column: keep the cursor in view (with a scroll-off) when it moved, or while it follows the top.
        let (n, ci) = {
            let s = self.browser.current.borrow();
            (s.rows.len(), s.index_of(cursor.as_deref()).unwrap_or(0))
        };
        if cwd_changed {
            self.cols[1].offset = 0;
        }
        if cwd_changed || cursor_changed || self.browser.pinned_to_top {
            self.cols[1].offset = scroll_to(self.cols[1].offset, ci, n, vis);
        } else {
            self.cols[1].offset = self.cols[1].offset.min(n.saturating_sub(vis));
        }
        // Parent column: cursor on the folder we are in.
        if let Some(p) = &self.browser.parent {
            let p = p.borrow();
            let pi = p.index_of(Some(paths::file_name(&cwd))).unwrap_or(0);
            let pv = self.visible_rows(lay.lists[0]);
            if cwd_changed {
                self.cols[0].offset = scroll_to(0, pi, p.rows.len(), pv);
            } else {
                self.cols[0].offset = self.cols[0].offset.min(p.rows.len().saturating_sub(pv));
            }
        }
        // Preview: starts at the top for every new previewed folder.
        let pv_path = self.browser.preview.as_ref().map(|p| p.borrow().path.clone());
        if pv_path != self.last_preview {
            self.cols[2].offset = 0;
        } else if let Some(p) = &self.browser.preview {
            let pv = self.visible_rows(lay.lists[2]);
            self.cols[2].offset = self.cols[2].offset.min(p.borrow().rows.len().saturating_sub(pv));
        }
        self.last_cwd = cwd;
        self.last_cursor = cursor;
        self.last_preview = pv_path;
        self.request_thumb();
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn ensure_timer(&mut self) {
        if !self.timer_on {
            unsafe { SetTimer(Some(self.hwnd), TIMER_ID, 1000 / 12, None) };
            self.timer_on = true;
        }
    }

    fn after_input(&mut self) {
        self.sync_view();
        self.invalidate();
        self.ensure_timer();
    }

    fn update_title(&self) {
        let loc = if self.browser.cwd.is_empty() { "This PC".to_string() } else { self.browser.cwd.clone() };
        let t: Vec<u16> = format!("{loc} \u{2014} SpaceMaster").encode_utf16().chain([0]).collect();
        unsafe {
            let _ = SetWindowTextW(self.hwnd, PCWSTR(t.as_ptr()));
        }
    }

    fn apply_theme(&mut self) {
        self.theme = Theme::current();
        let dark: i32 = self.theme.dark as i32;
        unsafe {
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const _, 4);
        }
        self.invalidate();
    }

    // MARK: Painting

    fn paint(&mut self) {
        if !self.gfx.begin() {
            return;
        }
        if self.viewer.active {
            let t = self.theme;
            self.viewer.paint(&mut self.gfx, &t);
            self.viewer.place_children(&self.gfx);
            if !self.gfx.end() {
                self.invalidate();
            }
            return;
        }
        let lay = self.layout();
        self.hits.clear();
        let t = self.theme;
        self.gfx.clear(t.window);
        self.paint_sidebar(&lay);
        self.paint_top(&lay);
        for col in 0..3 {
            self.paint_column(&lay, col);
        }
        if let Some(b) = lay.banner {
            self.paint_banner(b);
        }
        self.paint_status(&lay);
        // Separators.
        self.gfx.vline(SIDEBAR_W - 1.0, 0.0, self.gfx.height, t.separator);
        for c in 1..3 {
            self.gfx.vline(lay.lists[c].x, TOP_H, lay.status.y, t.separator);
        }
        self.gfx.hline(SIDEBAR_W, self.gfx.width, TOP_H - 1.0, t.separator);
        self.gfx.hline(SIDEBAR_W, self.gfx.width, lay.status.y, t.separator);
        self.paint_tooltip(&lay);
        if self.help {
            self.paint_help();
        }
        if !self.gfx.end() {
            self.invalidate();
        }
    }

    fn paint_sidebar(&mut self, lay: &Layout) {
        let t = self.theme;
        let g = &mut self.gfx;
        g.fill(lay.sidebar, t.sidebar);
        // Build the item list (cheap): favourites, then drives.
        let h = self.browser.model.home.clone();
        let mut items: Vec<SideItem> = Vec::new();
        let mut add = |title: &str, detail: String, path: String, glyph: &'static str, header: bool, fraction: f32| {
            items.push(SideItem { title: title.to_string(), detail, path, glyph, header, fraction, rect: Rect::new(0.0, 0.0, 0.0, 0.0) });
        };
        add("Favorites", String::new(), String::new(), "", true, 0.0);
        add("Home", String::new(), h.clone(), "\u{E80F}", false, 0.0);
        add("Desktop", String::new(), format!("{h}\\Desktop"), "\u{E7F4}", false, 0.0);
        add("Downloads", String::new(), format!("{h}\\Downloads"), "\u{E896}", false, 0.0);
        add("Documents", String::new(), format!("{h}\\Documents"), "\u{E8A5}", false, 0.0);
        add("This PC", String::new(), String::new(), "\u{E7F8}", false, 0.0);
        add("Drives", String::new(), String::new(), "", true, 0.0);
        for v in self.browser.model.volumes() {
            let detail = format!("{} free of {}", format::bytes(v.available), format::bytes(v.total));
            add(&v.display_name(), detail, v.path.clone(), "\u{EDA2}", false, v.fraction_used() as f32);
        }
        let mut y = 12.0;
        let cwd = self.browser.cwd.clone();
        // Best match: the longest sidebar path that contains the current folder.
        let mut best: Option<usize> = None;
        for (i, it) in items.iter().enumerate() {
            if it.header || (it.path.is_empty() && !(it.title == "This PC")) {
                continue;
            }
            let hit = if it.path.is_empty() { cwd.is_empty() } else { paths::is_within(&cwd, &it.path) };
            if hit && best.is_none_or(|b| items[b].path.len() <= it.path.len()) {
                best = Some(i);
            }
        }
        // Drives win over "Home" only when we are outside the home folder (handled by the longest match).
        for (i, it) in items.iter_mut().enumerate() {
            let hh = if it.header { 26.0 } else if it.fraction > 0.0 || it.glyph == "\u{EDA2}" { 50.0 } else { 30.0 };
            let r = Rect::new(8.0, y, SIDEBAR_W - 16.0, hh);
            it.rect = r;
            y += hh + if it.header { 0.0 } else { 2.0 };
            if it.header {
                g.text(&it.title.to_uppercase(), Face::Badge, Rect::new(16.0, r.y + 4.0, r.w - 16.0, 20.0), Align::Left, t.text3);
                continue;
            }
            let selected = best == Some(i);
            let hovered = self.hover == Some(Hit::Side(i));
            if selected {
                g.fill_round(r, 6.0, t.accent.alpha(0.20));
                g.fill_round(Rect::new(r.x, r.y + 7.0, 3.0, r.h - 14.0), 1.5, t.accent);
            } else if hovered {
                g.fill_round(r, 6.0, t.hover);
            }
            let is_drive = it.glyph == "\u{EDA2}";
            let icon_y = if is_drive { r.y + 4.0 } else { r.y };
            g.text(it.glyph, Face::Glyph, Rect::new(r.x + 8.0, icon_y, 22.0, if is_drive { 22.0 } else { r.h }), Align::Center, if selected { t.accent } else { t.text2 });
            g.text(&it.title, Face::Meta, Rect::new(r.x + 36.0, icon_y, r.w - 42.0, if is_drive { 22.0 } else { r.h }), Align::Left, t.text);
            if is_drive {
                let bar = Rect::new(r.x + 36.0, r.y + 28.0, r.w - 46.0, 5.0);
                g.fill_round(bar, 2.5, t.text.alpha(0.13));
                let col = if it.fraction > 0.9 { t.protected } else if it.fraction > 0.75 { t.mixed } else { t.accent };
                g.fill_round(Rect::new(bar.x, bar.y, (bar.w * it.fraction).max(5.0), 5.0), 2.5, col);
                g.text(&it.detail, Face::Small, Rect::new(r.x + 36.0, r.y + 33.0, r.w - 40.0, 16.0), Align::Left, t.text3);
            }
            self.hits.push((r, Hit::Side(i)));
        }
        self.side = items;
    }

    fn glyph_button(&mut self, r: Rect, glyph: &str, hit: Hit, enabled: bool, active: bool) {
        let t = self.theme;
        let hovered = self.hover == Some(hit) && enabled;
        if hovered {
            self.gfx.fill_round(r, 6.0, t.hover.alpha(1.6));
        }
        let c = if !enabled { t.text3.alpha(0.5) } else if active { t.accent } else { t.text };
        self.gfx.text(glyph, Face::Glyph, r, Align::Center, c);
        if enabled {
            self.hits.push((r, hit));
        }
    }

    fn paint_top(&mut self, lay: &Layout) {
        let t = self.theme;
        let top = lay.top;
        self.gfx.fill(top, t.bar);
        let mut x = top.x + 8.0;
        let by = top.y + 7.0;
        let (can_back, can_fwd, can_up) = (self.browser.can_go_back(), self.browser.can_go_forward(), !self.browser.cwd.is_empty());
        self.glyph_button(Rect::new(x, by, 30.0, 30.0), "\u{E72B}", Hit::Back, can_back, false);
        x += 32.0;
        self.glyph_button(Rect::new(x, by, 30.0, 30.0), "\u{E72A}", Hit::Forward, can_fwd, false);
        x += 32.0;
        self.glyph_button(Rect::new(x, by, 30.0, 30.0), "\u{E74A}", Hit::Up, can_up, false);
        x += 36.0;

        // Right side: filter field and action buttons.
        let mut rx = top.right() - 8.0;
        let hidden_on = self.browser.show_hidden;
        for (glyph, hit, active) in [("\u{E897}", Hit::Help, self.help), ("\u{E72C}", Hit::Rescan, false), ("\u{E7B3}", Hit::Hidden, hidden_on), ("\u{E8CB}", Hit::Sort, false)] {
            rx -= 32.0;
            self.glyph_button(Rect::new(rx, by, 30.0, 30.0), glyph, hit, true, active);
        }
        rx -= 8.0;
        let filter_w = 190.0_f32.min((top.w * 0.28).max(120.0));
        let frect = Rect::new(rx - filter_w, by, filter_w, 30.0);
        let filtering = matches!(self.browser.prompt.as_ref().map(|p| &p.kind), Some(PromptKind::Filter));
        let has_filter = !self.browser.filter.is_empty();
        self.gfx.fill_round(frect, 6.0, t.field);
        if filtering || has_filter {
            self.gfx.stroke_round(frect.inset(0.5), 6.0, t.accent, 1.0);
        }
        self.gfx.text("\u{E721}", Face::GlyphSmall, Rect::new(frect.x + 8.0, frect.y, 16.0, 30.0), Align::Center, t.text3);
        let (txt, col) = if filtering || has_filter {
            let s = if filtering { self.browser.prompt.as_ref().map(|p| p.text.clone()).unwrap_or_default() } else { self.browser.filter.clone() };
            (format!("{s}{}", if filtering { "\u{2502}" } else { "" }), t.text)
        } else {
            ("Filter  /".to_string(), t.text3)
        };
        self.gfx.text(&txt, Face::Meta, Rect::new(frect.x + 28.0, frect.y, frect.w - 34.0, 30.0), Align::Left, col);
        self.hits.push((frect, Hit::Filter));

        // Path bar / breadcrumb.
        let pr = Rect::new(x, by, (frect.x - 8.0 - x).max(40.0), 30.0);
        self.gfx.fill_round(pr, 6.0, t.field);
        let go_to = matches!(self.browser.prompt.as_ref().map(|p| &p.kind), Some(PromptKind::GoTo));
        if go_to {
            self.gfx.stroke_round(pr.inset(0.5), 6.0, t.accent, 1.0);
            let s = self.browser.prompt.as_ref().map(|p| p.text.clone()).unwrap_or_default();
            // Show the tail of long input.
            let wtxt = self.gfx.measure(&s, Face::Meta);
            let avail = pr.w - 20.0;
            let off = (wtxt - avail + 6.0).max(0.0);
            self.gfx.push_clip(pr.inset(2.0));
            self.gfx.text(&format!("{s}\u{2502}"), Face::Meta, Rect::new(pr.x + 10.0 - off, pr.y, wtxt + 40.0, 30.0), Align::Left, t.text);
            self.gfx.pop_clip();
            return;
        }
        self.hits.push((pr, Hit::PathBox));
        // Segments: This PC > C: > Users > me
        let mut segs: Vec<(String, String)> = vec![("This PC".into(), String::new())];
        if !self.browser.cwd.is_empty() {
            let mut acc = String::new();
            for (i, comp) in self.browser.cwd.split('\\').filter(|c| !c.is_empty()).enumerate() {
                acc = if i == 0 { format!("{comp}\\") } else { paths::join(&acc, comp) };
                segs.push((comp.to_string(), acc.clone()));
            }
        }
        let widths: Vec<f32> = segs.iter().map(|(n, _)| self.gfx.measure(n, Face::Meta) + 14.0).collect();
        let chev = 14.0;
        let mut start = 0usize;
        let mut total: f32 = widths.iter().sum::<f32>() + chev * (segs.len() as f32 - 1.0);
        while start + 1 < segs.len() && total > pr.w - 28.0 {
            total -= widths[start] + chev;
            start += 1;
        }
        let mut sx = pr.x + 6.0;
        if start > 0 {
            self.gfx.text("\u{2026}", Face::Meta, Rect::new(sx, pr.y, 14.0, 30.0), Align::Center, t.text3);
            sx += chev;
        }
        for i in start..segs.len() {
            let r = Rect::new(sx, pr.y + 3.0, widths[i], 24.0);
            let last = i + 1 == segs.len();
            if self.hover == Some(Hit::Path(i)) {
                self.gfx.fill_round(r, 5.0, t.hover.alpha(1.6));
            }
            self.gfx.text(&segs[i].0, Face::Meta, Rect::new(r.x + 7.0, r.y, r.w, r.h), Align::Left, if last { t.text } else { t.text2 });
            self.hits.push((r, Hit::Path(i)));
            sx += widths[i];
            if !last {
                self.gfx.text("\u{E76C}", Face::GlyphSmall, Rect::new(sx, pr.y, chev, 30.0), Align::Center, t.text3);
                sx += chev;
            }
        }
        // Remember the segment paths for click handling.
        self.path_segs = segs.into_iter().map(|s| s.1).collect();
    }

    fn paint_column(&mut self, lay: &Layout, col: usize) {
        let t = self.theme;
        let (hr, lr) = (lay.headers[col], lay.lists[col]);
        let state = match col {
            0 => self.browser.parent.clone(),
            1 => Some(self.browser.current.clone()),
            _ => self.browser.preview.clone(),
        };
        // Header
        self.gfx.fill(hr, t.window);
        let (title, detail) = self.column_header(col, state.as_ref().map(|s| s.borrow()).as_deref());
        let dw = self.gfx.measure(&detail, Face::Small);
        self.gfx.text(&detail, Face::Small, Rect::new(hr.right() - 14.0 - dw - 2.0, hr.y, dw + 4.0, hr.h), Align::Right, t.text2);
        let title_w = (hr.w - 28.0 - dw - 10.0).max(20.0);
        self.gfx.text(&title, Face::Header, Rect::new(hr.x + 14.0, hr.y, title_w, hr.h), Align::Left, if col == 1 { t.text } else { t.text2 });
        self.gfx.hline(hr.x, hr.right(), hr.bottom() - 1.0, t.separator);

        if col == 2 && state.is_none() {
            // File selected: info panel instead of a column.
            self.paint_info(lr);
            return;
        }
        let Some(state) = state else { return };
        let st = state.borrow();
        let (cursor, active, show_cursor) = match col {
            0 => (st.index_of(Some(paths::file_name(&self.browser.cwd))).unwrap_or(0), false, true),
            1 => (self.browser.cursor_index(), true, true),
            _ => (0, false, false),
        };
        let message: Option<String> = if let Some(e) = &st.error {
            Some(e.clone())
        } else if st.rows.is_empty() {
            Some(if st.entries.is_empty() { "Empty folder".into() } else { "No matching items".into() })
        } else {
            None
        };
        let empty_sel = HashSet::new();
        let sel = if col == 1 { &self.browser.selection } else { &empty_sel };
        let offset = self.cols[col].offset;
        let hover = self.hover_row.filter(|h| h.0 == col).map(|h| h.1);
        let spin = self.spinner;
        paint_rows(&mut self.gfx, &t, col, lr, &st, cursor, offset, sel, active, show_cursor, hover, spin, message.as_deref());
    }

    fn column_header(&self, col: usize, st: Option<&DirState>) -> (String, String) {
        let b = &self.browser;
        let name_of = |path: &str| -> String {
            if path.is_empty() {
                "This PC".to_string()
            } else if paths::is_drive_root(path) {
                b.model.volume_containing(path).map(|v| v.display_name()).unwrap_or_else(|| path.to_string())
            } else {
                paths::file_name(path).to_string()
            }
        };
        let total = |s: &DirState| format!("{}{}", format::bytes(s.total), if s.any_calculating { "+" } else { "" });
        match col {
            0 => match st {
                Some(s) => (name_of(&s.path), total(s)),
                None => (String::new(), String::new()),
            },
            1 => match st {
                Some(s) => (name_of(&s.path), total(s)),
                None => (String::new(), String::new()),
            },
            _ => match (st, b.cursor_row()) {
                (Some(s), _) => (name_of(&s.path), total(s)),
                (None, Some(r)) => (r.name.clone(), format::bytes(r.size)),
                _ => (String::new(), String::new()),
            },
        }
    }

    fn paint_info(&mut self, r: Rect) {
        let t = self.theme;
        let Some(row) = self.browser.cursor_row() else { return };
        let path = self.browser.path_of(&row.name);
        let ext = ext_of(&row.name);
        let g = &mut self.gfx;
        if let Some(ic) = g.icon_large(&ext) {
            g.draw_icon(&ic, Rect::new(r.x + 20.0, r.y + 20.0, 48.0, 48.0), 1.0);
        }
        let h = g.paragraph(&row.name, Face::Big, Rect::new(r.x + 20.0, r.y + 80.0, r.w - 40.0, 80.0), t.text);
        let mut y = r.y + 80.0 + h + 10.0;
        let mut line = |g: &mut Gfx, k: &str, v: &str| {
            g.text(k, Face::Small, Rect::new(r.x + 20.0, y, 80.0, 20.0), Align::Left, t.text3);
            g.text(v, Face::Meta, Rect::new(r.x + 104.0, y, r.w - 124.0, 20.0), Align::Left, t.text);
            y += 22.0;
        };
        line(g, "Size on disk", &format::bytes(row.size));
        line(g, "Modified", &shell::format_local_time(row.mtime));
        let kind = if row.kind == Kind::Symlink { "Link".to_string() } else if ext.is_empty() { "File".to_string() } else { format!("{} file", ext.to_uppercase()) };
        line(g, "Type", &kind);
        line(g, "Location", &self.browser.cwd);
        y += 8.0;
        let (color, glyph, label) = badge(&t, row.safety.level);
        let bw = g.measure(label, Face::Badge) + 34.0;
        g.fill_round(Rect::new(r.x + 20.0, y, bw, 22.0), 11.0, color.alpha(0.16));
        g.text(glyph, Face::GlyphSmall, Rect::new(r.x + 28.0, y, 14.0, 22.0), Align::Center, color);
        g.text(label, Face::Badge, Rect::new(r.x + 46.0, y, bw, 22.0), Align::Left, color);
        let rh = g.paragraph(&row.safety.reason, Face::Meta, Rect::new(r.x + 20.0, y + 30.0, r.w - 40.0, 120.0), t.text2);

        // Thumbnail below the info (delivered by the background worker; see request_thumb).
        let ty = y + 30.0 + rh + 18.0;
        let avail = Rect::new(r.x + 20.0, ty, r.w - 40.0, (r.bottom() - ty - 16.0).max(0.0));
        let key = format!("{path}|{}|{}", row.mtime, row.size);
        if let Some(th) = self.thumbs.get_mut(&key) {
            if th.w > 0 && avail.h > 40.0 && avail.w > 40.0 {
                let gen = g.generation();
                if th.bmp.as_ref().map(|b| b.0 != gen).unwrap_or(true) {
                    th.bmp = g.create_bitmap_bgra(th.w, th.h, &th.data).map(|b| (gen, b));
                }
                if let Some((_, bmp)) = &th.bmp {
                    let s = g.dpi / 96.0;
                    let (nw, nh) = (th.w as f32 / s, th.h as f32 / s);
                    let k = (avail.w / nw).min(avail.h / nh).min(1.0);
                    let dest = Rect::new(g.snap(avail.x), g.snap(avail.y), nw * k, nh * k);
                    g.fill_round(dest.inset(-1.0), 5.0, t.panel);
                    g.draw_scaled(bmp, dest);
                    g.stroke_round(dest.inset(-0.5), 4.0, t.separator, 1.0);
                }
            }
        }
    }

    fn paint_banner(&mut self, b: Rect) {
        let t = self.theme;
        let n = self.browser.model.scanner.denied();
        self.gfx.fill(b, t.mixed.alpha(0.16));
        self.gfx.hline(b.x, b.right(), b.y, t.separator);
        self.gfx.text(
            &format!("{n} folders couldn't be read. Restart as administrator to include them."),
            Face::Meta,
            Rect::new(b.x + 14.0, b.y, b.w - 220.0, b.h),
            Align::Left,
            t.text,
        );
        let label = "Restart as administrator";
        let bw = self.gfx.measure(label, Face::Meta) + 24.0;
        let br = Rect::new(b.right() - 40.0 - bw, b.y + 4.0, bw, b.h - 8.0);
        let hov = self.hover == Some(Hit::BannerAction);
        self.gfx.fill_round(br, 5.0, if hov { t.accent } else { t.accent.alpha(0.85) });
        self.gfx.text(label, Face::Meta, br, Align::Center, t.on_accent);
        self.hits.push((br, Hit::BannerAction));
        let cr = Rect::new(b.right() - 32.0, b.y + 3.0, 26.0, b.h - 6.0);
        self.glyph_button(cr, "\u{E711}", Hit::BannerClose, true, false);
    }

    fn paint_status(&mut self, lay: &Layout) {
        let t = self.theme;
        let s = lay.status;
        self.gfx.fill(s, t.bar);
        let mut x = s.x + 14.0;
        if let Some(v) = self.browser.model.volume_containing(&self.browser.cwd) {
            let bar = Rect::new(x, s.y + 11.0, 64.0, 6.0);
            self.gfx.fill_round(bar, 3.0, t.text.alpha(0.13));
            let f = v.fraction_used() as f32;
            let col = if f > 0.9 { t.protected } else if f > 0.75 { t.mixed } else { t.accent };
            self.gfx.fill_round(Rect::new(bar.x, bar.y, (bar.w * f).max(6.0), 6.0), 3.0, col);
            x += 74.0;
            let txt = format!("{}: {} free of {}", v.letter, format::bytes(v.available), format::bytes(v.total));
            let w = self.gfx.measure(&txt, Face::Small) + 6.0;
            self.gfx.text(&txt, Face::Small, Rect::new(x, s.y, w, s.h), Align::Left, t.text2);
            x += w + 18.0;
        }
        // Right side: flash > scanning > nothing.
        let right_text = if let Some(f) = &self.browser.flash {
            Some((f.text.clone(), t.text))
        } else {
            self.browser.scan_text().map(|s| (s, t.text2))
        };
        let mut rx = s.right() - 14.0;
        if let Some((txt, col)) = right_text {
            let w = (self.gfx.measure(&txt, Face::Small) + 6.0).min((s.w * 0.55).max(120.0));
            self.gfx.text(&txt, Face::Small, Rect::new(rx - w, s.y, w, s.h), Align::Right, col);
            rx -= w + 14.0;
            if self.browser.is_busy() {
                let c = (rx - 2.0, s.y + s.h / 2.0);
                self.gfx.arc(c, 5.0, self.spinner * 360.0, 250.0, t.text2, 1.6);
                rx -= 18.0;
            }
        }
        let summary = self.browser.summary();
        let w = (rx - x - 8.0).max(40.0);
        self.gfx.text(&summary, Face::Small, Rect::new(x, s.y, w, s.h), Align::Left, t.text);
    }

    fn paint_tooltip(&mut self, lay: &Layout) {
        let Some((col, idx)) = self.hover_row else { return };
        if col != 1 {
            return;
        }
        let list = lay.lists[1];
        if self.mouse.0 < list.right() - 230.0 {
            return;
        }
        let reason = {
            let s = self.browser.current.borrow();
            match s.rows.get(idx) {
                Some(r) => r.safety.reason.to_string(),
                None => return,
            }
        };
        let t = self.theme;
        let wmax = 300.0;
        let x = (self.mouse.0 - wmax - 20.0).max(list.x + 8.0);
        let y = self.mouse.1 + 18.0;
        let h = self.gfx.paragraph_height(&reason, Face::Meta, wmax - 20.0);
        let r = Rect::new(x, (y).min(self.gfx.height - h - 40.0), wmax, h + 16.0);
        self.gfx.fill_round(r.inset(-1.0), 7.0, Color::rgba(0x000000, 0.25));
        self.gfx.fill_round(r, 6.0, t.panel);
        self.gfx.stroke_round(r.inset(0.5), 6.0, t.separator, 1.0);
        self.gfx.paragraph(&reason, Face::Meta, Rect::new(r.x + 10.0, r.y + 8.0, r.w - 20.0, h), t.text);
    }

    fn paint_help(&mut self) {
        let t = self.theme;
        let (w, h) = (self.gfx.width, self.gfx.height);
        self.gfx.fill(Rect::new(0.0, 0.0, w, h), Color::rgba(0x000000, 0.45));
        let pw = 640.0_f32.min(w - 40.0);
        let ph = 530.0_f32.min(h - 40.0);
        let r = Rect::new((w - pw) / 2.0, (h - ph) / 2.0, pw, ph);
        self.gfx.fill_round(r, 10.0, t.panel);
        self.gfx.stroke_round(r.inset(0.5), 10.0, t.separator, 1.0);
        self.gfx.text("Keyboard shortcuts", Face::Big, Rect::new(r.x + 24.0, r.y + 14.0, r.w - 48.0, 36.0), Align::Left, t.text);
        const ROWS: [(&str, &str); 24] = [
            ("j k  \u{2191} \u{2193}", "move cursor"),
            ("h l  \u{2190} \u{2192}  Bksp", "parent / enter folder"),
            ("gg  G  Ctrl+d  Ctrl+u", "top / bottom / half page"),
            ("H L  Alt+\u{2190} Alt+\u{2192}", "history back / forward"),
            ("Enter  o", "open file (folders: enter)"),
            ("Tab  v  Ctrl+A", "select + next / select all"),
            ("Space", "open the viewer (image, video, PDF, text, code, HTML ...)"),
            ("d  Del", "move to Recycle Bin (Ctrl+Z undo)"),
            ("D  Shift+Del", "delete permanently"),
            (".", "show / hide hidden files"),
            (",s ,n ,m", "sort by size / name / date (capital reverses)"),
            ("gh gd gD gc", "Home, Downloads, Desktop, Documents"),
            ("ga gl gT gr", "Program Files, AppData\\Local, Temp, This PC"),
            ("gt", "open the Recycle Bin"),
            ("g\u{2423}  Ctrl+L", "go to path (~ and %VARS% work)"),
            ("/  f  Ctrl+F", "filter the current folder"),
            ("r  F5  Ctrl+R", "rescan the folder under the cursor"),
            ("y", "copy path"),
            ("Ctrl+C", "copy the files"),
            ("R", "reveal in File Explorer"),
            ("Esc", "clear selection / filter"),
            ("?  F1", "this help"),
            ("Ctrl +  Ctrl -  Ctrl 0", "zoom in / out / reset (also Ctrl + wheel)"),
            ("Mouse", "click, double-click, Ctrl/Shift-click, wheel, right-click"),
        ];
        let mut y = r.y + 58.0;
        for (k, d) in ROWS {
            self.gfx.text(k, Face::Meta, Rect::new(r.x + 24.0, y, 240.0, 18.0), Align::Left, t.accent);
            self.gfx.text(d, Face::Meta, Rect::new(r.x + 274.0, y, r.w - 298.0, 18.0), Align::Left, t.text);
            y += 18.5;
        }
    }

    // MARK: Hit testing

    fn hit_at(&self, x: f32, y: f32) -> Option<Hit> {
        self.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, h)| *h)
    }

    fn row_at(&self, lay: &Layout, x: f32, y: f32) -> Option<(usize, usize)> {
        for col in 0..3 {
            let l = lay.lists[col];
            if l.contains(x, y) {
                let idx = self.cols[col].offset + ((y - l.y) / ROW_H) as usize;
                return Some((col, idx));
            }
        }
        None
    }

    fn row_count(&self, col: usize) -> usize {
        match col {
            0 => self.browser.parent.as_ref().map(|p| p.borrow().rows.len()).unwrap_or(0),
            1 => self.browser.current.borrow().rows.len(),
            _ => self.browser.preview.as_ref().map(|p| p.borrow().rows.len()).unwrap_or(0),
        }
    }

    // MARK: Input

    fn mouse_move(&mut self, x: f32, y: f32) {
        self.mouse = (x, y);
        let lay = self.layout();
        let hit = self.hit_at(x, y);
        let row = self.row_at(&lay, x, y).filter(|(c, i)| *i < self.row_count(*c));
        let tooltip_zone = row.is_some_and(|(c, _)| c == 1) && x > lay.lists[1].right() - 230.0;
        if hit != self.hover || row != self.hover_row || tooltip_zone {
            self.hover = hit;
            self.hover_row = row;
            self.invalidate();
        }
        if !self.tracking {
            let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: self.hwnd, dwHoverTime: 0 };
            unsafe {
                let _ = TrackMouseEvent(&mut tme);
            }
            self.tracking = true;
        }
    }

    fn mouse_leave(&mut self) {
        self.tracking = false;
        self.hover = None;
        self.hover_row = None;
        self.invalidate();
    }

    fn click(&mut self, x: f32, y: f32, double: bool) {
        if self.help {
            self.help = false;
            self.invalidate();
            return;
        }
        let lay = self.layout();
        if let Some(h) = self.hit_at(x, y) {
            match h {
                Hit::Back => self.browser.go_back(),
                Hit::Forward => self.browser.go_forward(),
                Hit::Up => self.browser.leave(),
                Hit::Filter => self.browser.start_prompt(PromptKind::Filter),
                Hit::PathBox => self.browser.start_prompt(PromptKind::GoTo),
                Hit::Path(i) => {
                    if let Some(p) = self.path_segs.get(i).cloned() {
                        self.browser.cd(&p, None, true);
                    }
                }
                Hit::Sort => {
                    self.show_sort_menu();
                    return;
                }
                Hit::Hidden => self.browser.toggle_hidden(),
                Hit::Rescan => self.browser.rescan(),
                Hit::Help => self.help = true,
                Hit::Side(i) => {
                    if let Some(it) = self.side.get(i) {
                        let p = it.path.clone();
                        self.browser.cd(&p, None, true);
                    }
                }
                Hit::BannerAction => {
                    if shell::relaunch_as_admin(self.hwnd) {
                        unsafe { PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)).ok() };
                    }
                }
                Hit::BannerClose => self.banner_dismissed = true,
            }
            self.after_input();
            return;
        }
        if let Some((col, idx)) = self.row_at(&lay, x, y) {
            if idx < self.row_count(col) {
                let (ctrl, shift) = (key_down(VK_CONTROL.0 as i32), key_down(VK_SHIFT.0 as i32));
                match col {
                    0 => self.browser.click_parent_row(idx),
                    1 => {
                        self.browser.click_row(idx, ctrl, shift);
                        if double {
                            self.browser.open_cursor();
                        }
                    }
                    _ => self.browser.click_preview_row(idx),
                }
            }
            self.after_input();
        }
    }

    fn wheel(&mut self, x: f32, y: f32, delta: i32) {
        let lay = self.layout();
        let Some((col, _)) = self.row_at(&lay, x, y) else { return };
        self.wheel[col] -= delta as f32 / 120.0 * 3.0;
        let rows = self.wheel[col].trunc() as isize;
        if rows == 0 {
            return;
        }
        self.wheel[col] -= rows as f32;
        let vis = self.visible_rows(lay.lists[col]);
        let max = self.row_count(col).saturating_sub(vis);
        let off = (self.cols[col].offset as isize + rows).clamp(0, max as isize) as usize;
        if off != self.cols[col].offset {
            self.cols[col].offset = off;
            let row = self.row_at(&lay, x, y).filter(|(c, i)| *i < self.row_count(*c));
            self.hover_row = row;
            self.invalidate();
        }
    }

    fn show_sort_menu(&mut self) {
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let b = &self.browser;
            for (id, key) in [(1usize, SortKey::Size), (2, SortKey::Name), (3, SortKey::Modified)] {
                let label: Vec<u16> = key.title().encode_utf16().chain([0]).collect();
                let flags = MF_STRING | if b.sort == key { MF_CHECKED } else { MF_UNCHECKED };
                let _ = AppendMenuW(menu, flags, id, PCWSTR(label.as_ptr()));
            }
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING | if b.sort_reversed { MF_CHECKED } else { MF_UNCHECKED }, 4, w!("Reverse order"));
            let r = self.hits.iter().find(|(_, h)| *h == Hit::Sort).map(|(r, _)| *r).unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0));
            let s = self.gfx.dpi / 96.0;
            let mut pt = POINT { x: (r.x * s) as i32, y: (r.bottom() * s) as i32 };
            let _ = windows::Win32::Graphics::Gdi::ClientToScreen(self.hwnd, &mut pt);
            let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN, pt.x, pt.y, Some(0), self.hwnd, None);
            let _ = DestroyMenu(menu);
            match cmd.0 {
                1 => self.browser.sort_by_keep(SortKey::Size),
                2 => self.browser.sort_by_keep(SortKey::Name),
                3 => self.browser.sort_by_keep(SortKey::Modified),
                4 => {
                    let (k, r) = (self.browser.sort, !self.browser.sort_reversed);
                    self.browser.set_sort(k, r);
                }
                _ => {}
            }
        }
        self.after_input();
    }

    fn context_menu(&mut self, x: f32, y: f32) {
        let lay = self.layout();
        if let Some((1, idx)) = self.row_at(&lay, x, y) {
            if idx < self.row_count(1) {
                self.browser.click_row(idx, false, false);
                self.sync_view();
                self.invalidate();
            }
        } else {
            return;
        }
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let b = &self.browser;
            let has = !b.targets().is_empty();
            let dis = |on: bool| if on { MF_ENABLED } else { MF_GRAYED };
            let _ = AppendMenuW(menu, MF_STRING | dis(has), 1, w!("Open"));
            let _ = AppendMenuW(menu, MF_STRING | dis(has), 2, w!("Show in File Explorer"));
            let _ = AppendMenuW(menu, MF_STRING | dis(has), 3, w!("Copy path"));
            let _ = AppendMenuW(menu, MF_STRING | dis(has), 4, w!("Copy"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING, 5, w!("Rescan"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING | dis(has && b.deleting == 0), 6, w!("Move to Recycle Bin\tDel"));
            let _ = AppendMenuW(menu, MF_STRING | dis(has && b.deleting == 0), 7, w!("Delete permanently\u{2026}\tShift+Del"));
            let _ = AppendMenuW(menu, MF_STRING | dis(b.can_undo()), 8, w!("Undo\tCtrl+Z"));
            let s = self.gfx.dpi / 96.0;
            let mut pt = POINT { x: (x * s) as i32, y: (y * s) as i32 };
            let _ = windows::Win32::Graphics::Gdi::ClientToScreen(self.hwnd, &mut pt);
            let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN, pt.x, pt.y, Some(0), self.hwnd, None);
            let _ = DestroyMenu(menu);
            match cmd.0 {
                1 => self.browser.open_cursor(),
                2 => self.browser.reveal(),
                3 => self.browser.copy_path(),
                4 => self.browser.copy_files(),
                5 => self.browser.rescan(),
                6 => self.browser.request_delete(false),
                7 => self.browser.request_delete(true),
                8 => self.browser.undo(),
                _ => {}
            }
        }
        self.after_input();
    }

    fn key_down(&mut self, vk: u16, repeat: bool) -> bool {
        let ctrl = key_down(VK_CONTROL.0 as i32);
        let shift = key_down(VK_SHIFT.0 as i32);
        let alt = key_down(VK_MENU.0 as i32);
        if self.help {
            self.help = false;
            self.invalidate();
            return true;
        }
        if self.viewer.active {
            if ctrl && !alt {
                if let Some(d) = zoom_action(vk) {
                    self.viewer.zoom_key(d);
                    self.invalidate();
                    return true;
                }
            }
            let key = match VIRTUAL_KEY(vk) {
                VK_UP => Key::Up,
                VK_DOWN => Key::Down,
                VK_LEFT => Key::Left,
                VK_RIGHT => Key::Right,
                VK_ESCAPE => Key::Escape,
                VK_PRIOR => Key::PageUp,
                VK_NEXT => Key::PageDown,
                VK_HOME => Key::Home,
                VK_END => Key::End,
                v if ctrl && v.0 == b'C' as u16 => Key::Char('c'),
                _ => return false,
            };
            let act = self.viewer.key(KeyEvent { key, ctrl, shift, alt }, vk, repeat);
            self.apply_viewer_act(act);
            return true;
        }
        if ctrl && !alt {
            if let Some(d) = zoom_action(vk) {
                self.change_zoom(d);
                return true;
            }
        }
        let key = match VIRTUAL_KEY(vk) {
            VK_UP => Key::Up,
            VK_DOWN => Key::Down,
            VK_LEFT => Key::Left,
            VK_RIGHT => Key::Right,
            VK_RETURN => Key::Enter,
            VK_ESCAPE => Key::Escape,
            VK_BACK => Key::Backspace,
            VK_DELETE => Key::Delete,
            VK_TAB => Key::Tab,
            VK_PRIOR => Key::PageUp,
            VK_NEXT => Key::PageDown,
            VK_HOME => Key::Home,
            VK_END => Key::End,
            VK_F5 => Key::F5,
            VK_F1 => {
                self.help = true;
                self.invalidate();
                return true;
            }
            v if ctrl && !alt && (b'A' as u16..=b'Z' as u16).contains(&v.0) => {
                let c = (v.0 as u8 as char).to_ascii_lowercase();
                if c == 'v' && self.browser.prompt.is_some() {
                    if let Some(t) = shell::get_clipboard_text(self.hwnd) {
                        self.browser.prompt_insert(&t);
                        self.after_input();
                    }
                    return true;
                }
                Key::Char(c)
            }
            _ => return false,
        };
        let handled = self.browser.handle_key(KeyEvent { key, ctrl, shift, alt });
        self.after_input();
        handled
    }

    fn char_input(&mut self, unit: u16, repeat: bool) {
        if key_down(VK_CONTROL.0 as i32) && !key_down(VK_MENU.0 as i32) {
            return;
        }
        if unit < 0x20 || unit == 0x7F {
            return;
        }
        let c = if (0xD800..0xDC00).contains(&unit) {
            self.high_surrogate = Some(unit);
            return;
        } else if (0xDC00..0xE000).contains(&unit) {
            match self.high_surrogate.take() {
                Some(h) => char::decode_utf16([h, unit]).next().and_then(|r| r.ok()),
                None => None,
            }
        } else {
            char::from_u32(unit as u32)
        };
        let Some(c) = c else { return };
        if self.help {
            if c == '?' || c == 'q' {
                self.help = false;
                self.invalidate();
            }
            return;
        }
        if self.viewer.active {
            let act = self.viewer.ch(c, repeat);
            self.apply_viewer_act(act);
            return;
        }
        self.browser.handle_key(KeyEvent::ch(c));
        self.after_input();
    }

    fn open_viewer(&mut self, path: &str) {
        let dark = self.theme.dark;
        self.viewer.open(self.hwnd.0 as isize, path, dark);
        self.ensure_timer();
        self.invalidate();
    }

    fn apply_viewer_act(&mut self, act: Act) {
        match act {
            Act::None => {}
            Act::Close => {
                if self.viewer.fullscreen {
                    self.toggle_fullscreen();
                }
                self.viewer.close();
                self.sync_view();
            }
            Act::Step(d) => self.viewer_step(d),
            Act::Maximize => unsafe {
                let _ = ShowWindow(self.hwnd, if IsZoomed(self.hwnd).as_bool() { SW_RESTORE } else { SW_MAXIMIZE });
            },
            Act::Fullscreen => self.toggle_fullscreen(),
            Act::Copy(text) => {
                shell::set_clipboard_text(self.hwnd, &text);
            }
        }
        self.invalidate();
        self.ensure_timer();
    }

    /// n / p: the next / previous file (not folder) in the current list.
    fn viewer_step(&mut self, dir: i32) {
        let n = self.browser.current.borrow().rows.len() as i32;
        let mut i = self.browser.cursor_index() as i32 + dir;
        while i >= 0 && i < n {
            let (is_dir, name) = {
                let s = self.browser.current.borrow();
                (s.rows[i as usize].is_dir(), s.rows[i as usize].name.clone())
            };
            if !is_dir {
                self.browser.move_cursor(i as isize);
                let p = self.browser.path_of(&name);
                self.open_viewer(&p);
                return;
            }
            i += dir;
        }
    }

    fn toggle_fullscreen(&mut self) {
        unsafe {
            if let Some((style, wp)) = self.fs_saved.take() {
                SetWindowLongPtrW(self.hwnd, GWL_STYLE, style);
                let _ = SetWindowPlacement(self.hwnd, &wp);
                let _ = SetWindowPos(self.hwnd, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED);
                self.viewer.fullscreen = false;
            } else {
                let style = GetWindowLongPtrW(self.hwnd, GWL_STYLE);
                let mut wp = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
                let _ = GetWindowPlacement(self.hwnd, &mut wp);
                let mon = windows::Win32::Graphics::Gdi::MonitorFromWindow(self.hwnd, windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTONEAREST);
                let mut mi = windows::Win32::Graphics::Gdi::MONITORINFO { cbSize: std::mem::size_of::<windows::Win32::Graphics::Gdi::MONITORINFO>() as u32, ..Default::default() };
                if windows::Win32::Graphics::Gdi::GetMonitorInfoW(mon, &mut mi).as_bool() {
                    SetWindowLongPtrW(self.hwnd, GWL_STYLE, style & !(WS_OVERLAPPEDWINDOW.0 as isize));
                    let r = mi.rcMonitor;
                    let _ = SetWindowPos(self.hwnd, Some(HWND_TOP), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_FRAMECHANGED | SWP_NOACTIVATE);
                    self.fs_saved = Some((style, wp));
                    self.viewer.fullscreen = true;
                }
            }
        }
    }

    fn timer(&mut self) {
        if self.viewer.active {
            if self.viewer.tick() {
                self.invalidate();
            }
            if !self.viewer.needs_timer() {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER_ID);
                }
                self.timer_on = false;
            }
            return;
        }
        let dirty = self.browser.poll();
        let busy = self.browser.is_busy() || self.browser.any_calculating();
        if busy {
            self.spinner = (self.spinner + 1.0 / 12.0 * 1.1).fract();
        }
        if dirty || busy {
            self.sync_view();
            self.invalidate();
        }
        if !busy && self.browser.flash.is_none() && !dirty {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_ID);
            }
            self.timer_on = false;
            self.browser.model.refresh_volumes();
            self.invalidate();
        }
    }

    /// `dir`: +1 zoom in, -1 zoom out, 0 reset. Steps of 10 %, between 60 % and 250 %.
    fn change_zoom(&mut self, dir: i8) {
        let cur = self.gfx.zoom();
        let z = match dir {
            0 => 1.0,
            d if d > 0 => (cur + 0.1).min(2.5),
            _ => (cur - 0.1).max(0.6),
        };
        let z = (z * 100.0).round() / 100.0;
        self.gfx.set_zoom(z);
        self.browser.settings.set("zoom", z);
        self.browser.set_flash(format!("Zoom {}%", (z * 100.0).round() as i32), 2);
        self.sync_view();
        self.invalidate();
        self.ensure_timer();
    }

    fn save_placement(&mut self) {
        let mut wp = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
        if unsafe { GetWindowPlacement(self.hwnd, &mut wp) }.is_ok() {
            let r = wp.rcNormalPosition;
            let s = &mut self.browser.settings;
            s.set("win.x", r.left);
            s.set("win.y", r.top);
            s.set("win.w", r.right - r.left);
            s.set("win.h", r.bottom - r.top);
            s.set("win.max", wp.showCmd == SW_SHOWMAXIMIZED.0 as u32);
        }
        self.browser.settings.save();
    }
}

/// Ctrl+Plus / Ctrl+Minus / Ctrl+0 on any keyboard layout: the key is translated to the character it produces
/// (with Shift, without Ctrl), so "+" works wherever the layout puts it.
fn zoom_action(vk: u16) -> Option<i8> {
    match VIRTUAL_KEY(vk) {
        VK_ADD => return Some(1),
        VK_SUBTRACT => return Some(-1),
        VK_NUMPAD0 => return Some(0),
        _ => {}
    }
    if vk == 0x30 {
        return Some(0);
    }
    let mut st = [0u8; 256];
    unsafe {
        if GetKeyboardState(&mut st).is_ok() {
            for k in [VK_CONTROL, VK_LCONTROL, VK_RCONTROL] {
                st[k.0 as usize] = 0;
            }
            let mut buf = [0u16; 4];
            if ToUnicode(vk as u32, 0, Some(&st), &mut buf, 4) == 1 {
                return match char::from_u32(buf[0] as u32) {
                    Some('+') | Some('=') => Some(1),
                    Some('-') | Some('_') => Some(-1),
                    _ => None,
                };
            }
        }
    }
    match VIRTUAL_KEY(vk) {
        VK_OEM_PLUS => Some(1),
        VK_OEM_MINUS => Some(-1),
        _ => None,
    }
}

fn key_down(vk: i32) -> bool {
    unsafe { GetKeyState(vk) < 0 }
}

fn scroll_to(offset: usize, cursor: usize, n: usize, vis: usize) -> usize {
    let so = 4.min(vis.saturating_sub(1) / 2);
    let mut off = offset as isize;
    let (c, v) = (cursor as isize, vis as isize);
    if c < off + so as isize {
        off = c - so as isize;
    } else if c > off + v - 1 - so as isize {
        off = c - (v - 1 - so as isize);
    }
    off.clamp(0, (n as isize - v).max(0)) as usize
}

fn ext_of(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 && name.len() - i <= 12 => name[i + 1..].to_ascii_lowercase(),
        _ => String::new(),
    }
}

fn badge(t: &Theme, level: Level) -> (Color, &'static str, &'static str) {
    match level {
        Level::Safe => (t.safe, "\u{E73E}", "Safe"),
        Level::Protected => (t.protected, "\u{E72E}", "Protected"),
        Level::Mixed => (t.mixed, "\u{E785}", "Has safe items"),
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_rows(
    g: &mut Gfx,
    t: &Theme,
    col: usize,
    rect: Rect,
    st: &DirState,
    cursor: usize,
    offset: usize,
    selected: &HashSet<String>,
    active: bool,
    show_cursor: bool,
    hover: Option<usize>,
    spinner: f32,
    message: Option<&str>,
) {
    g.fill(rect, t.window);
    g.push_clip(rect);
    if st.rows.is_empty() {
        if let Some(m) = message {
            g.paragraph_centered(m, Face::Meta, Rect::new(rect.x + 16.0, rect.y + 28.0, rect.w - 32.0, 80.0), t.text2);
        }
        g.pop_clip();
        return;
    }
    let rows = &st.rows;
    let vis = ((rect.h / ROW_H).ceil() as usize) + 1;
    let last = (offset + vis).min(rows.len());
    let width = rect.w;
    let show_pct = width >= 300.0;
    let compact = width < 400.0;
    let total = st.total.max(1) as f64;
    let max_size = st.max_size.max(1) as f64;
    let bar_max = width - 12.0;
    let dark = t.dark;
    let _ = col;
    for i in offset..last {
        let r = &rows[i];
        let y = rect.y + (i - offset) as f32 * ROW_H;
        let is_cursor = show_cursor && i == cursor;
        let is_sel = selected.contains(&r.name);

        // Background layers first (hover, selection, cursor), the size bar on top so it is never hidden.
        if hover == Some(i) && !is_cursor {
            g.fill_round(Rect::new(rect.x + 4.0, y + 1.0, width - 8.0, ROW_H - 2.0), 6.0, t.hover);
        }
        if is_sel {
            g.fill_round(Rect::new(rect.x + 3.0, y + 1.0, width - 6.0, ROW_H - 2.0), 7.0, t.accent.alpha(0.20));
            g.fill_round(Rect::new(rect.x + 1.0, y + 6.0, 3.0, ROW_H - 12.0), 1.5, t.accent);
        }
        if is_cursor {
            // Light tint plus an outline: the cursor stays obvious without covering the bar colour.
            let cr = Rect::new(rect.x + 5.0, y + 1.0, width - 10.0, ROW_H - 2.0);
            if active {
                g.fill_round(cr, 6.0, t.accent.alpha(0.22));
                g.stroke_round(cr.inset(0.75), 6.0, t.accent, 1.5);
            } else {
                g.fill_round(cr, 6.0, t.text.alpha(0.10));
                g.stroke_round(cr.inset(0.75), 6.0, t.text3, 1.0);
            }
        }
        if r.size > 0 {
            let share = r.size as f64 / total;
            let frac = r.size as f64 / max_size;
            let bw = (bar_max as f64 * frac).max(6.0) as f32;
            let bucket = share_bucket(share);
            let base = t.bar(share_hue(bucket));
            let (a0, a1) = if dark { (0.92, 0.42) } else { (0.60, 0.28) };
            g.gradient_bar(Rect::new(rect.x + 6.0, y + 2.0, bw, ROW_H - 4.0), 6.0, bucket as u32, dark, base.alpha(a0), base.alpha(a1));
        }
        let on_accent = false;
        let primary = if on_accent { t.on_accent } else { t.text };
        let secondary = if on_accent { t.on_accent.alpha(0.8) } else { t.text2 };
        let tertiary = if on_accent { t.on_accent.alpha(0.65) } else { t.text3 };

        // Icon
        let key = match r.kind {
            Kind::Directory => "<dir>".to_string(),
            Kind::Symlink if r.attrs & 0x10 != 0 => "<dir>".to_string(),
            _ => ext_of(&r.name),
        };
        if let Some(ic) = g.icon(&key) {
            g.draw_icon(&ic, Rect::new(rect.x + 14.0, y, 16.0, ROW_H), if r.hidden { 0.6 } else { 1.0 });
        }

        // Right side, laid out from the right edge.
        let mut x = rect.right() - 12.0 - 16.0;
        if r.is_dir() {
            g.text("\u{E76C}", Face::GlyphSmall, Rect::new(rect.right() - 12.0 - 12.0, y, 12.0, ROW_H), Align::Center, tertiary);
        }
        if show_pct {
            if r.size > 0 && !r.unknown_size {
                g.text(&format::percent(r.size as f64 / total), Face::Small, Rect::new(x - 40.0, y, 40.0, ROW_H), Align::Right, tertiary);
            }
            x -= 40.0;
        }
        let size_text = if r.unknown_size { "\u{2014}".to_string() } else { format::bytes(r.size) };
        g.text(&size_text, Face::Meta, Rect::new(x - 66.0, y, 66.0, ROW_H), Align::Right, if r.calculating { secondary } else { primary });
        if r.calculating {
            let w = g.measure(&size_text, Face::Meta);
            g.arc((x - w - 9.0, y + ROW_H / 2.0), 4.0, spinner * 360.0, 240.0, secondary, 1.6);
        }
        x -= 66.0 + if r.calculating { 12.0 } else { 0.0 };

        // Badge
        let (color, glyph, label) = badge(t, r.safety.level);
        let bh = 17.0;
        let lw = if compact { 0.0 } else { g.measure(label, Face::Badge) };
        let bw = if compact { bh + 2.0 } else { 8.0 + 11.0 + 4.0 + lw + 8.0 };
        let br = Rect::new(x - bw, y + (ROW_H - bh) / 2.0, bw, bh);
        g.fill_round(br, bh / 2.0, if on_accent { t.on_accent.alpha(0.22) } else { color.alpha(0.16) });
        let bc = if on_accent { t.on_accent } else { color };
        let gx = if compact { br.x + (bw - 11.0) / 2.0 } else { br.x + 8.0 };
        g.text(glyph, Face::GlyphSmall, Rect::new(gx, br.y, 11.0, bh), Align::Center, bc);
        if !compact {
            g.text(label, Face::Badge, Rect::new(br.x + 23.0, br.y, lw + 4.0, bh), Align::Left, bc);
        }
        x -= bw + 8.0;

        // Name
        let name_col = if r.hidden && !on_accent {
            secondary
        } else if r.kind == Kind::Symlink && !on_accent {
            if dark { Color::rgb(0x40C8E0) } else { Color::rgb(0x0E8A99) }
        } else {
            primary
        };
        g.text(&r.name, Face::Name, Rect::new(rect.x + 38.0, y, (x - rect.x - 38.0).max(30.0), ROW_H), Align::Left, name_col);
    }
    // Scroll thumb
    let n = rows.len();
    let vis_full = (rect.h / ROW_H).floor() as usize;
    if n > vis_full && vis_full > 0 {
        let h = rect.h;
        let thumb = (h * vis_full as f32 / n as f32).max(24.0);
        let max_off = (n - vis_full) as f32;
        let ty = (h - thumb) * (offset as f32 / max_off).min(1.0);
        g.fill_round(Rect::new(rect.right() - 6.0, rect.y + ty + 2.0, 3.5, thumb - 4.0), 1.75, t.text.alpha(0.22));
    }
    g.pop_clip();
}

// MARK: Window procedure

pub fn run() -> i32 {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let class = w!("SpaceMasterWindow");
        let icon = LoadIconW(Some(hinst.into()), PCWSTR(1 as *const u16)).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS | CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            hIcon: icon,
            hIconSm: icon,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);

        let selftest_out = std::env::args().skip_while(|a| a != "--selftest").nth(1);
        let mut settings = match std::env::var_os("SM_SETTINGS_FILE") {
            Some(p) => Settings::load(Some(p.into())),
            None => Settings::load_default(),
        };
        if let Ok(p) = std::env::var("SM_START_PATH") {
            settings.set("lastPath", p);
        }
        let (x, y, ww, hh) = (
            settings.get_i64("win.x").map(|v| v as i32).unwrap_or(CW_USEDEFAULT),
            settings.get_i64("win.y").map(|v| v as i32).unwrap_or(CW_USEDEFAULT),
            settings.get_i64("win.w").map(|v| v as i32).unwrap_or(1280),
            settings.get_i64("win.h").map(|v| v as i32).unwrap_or(780),
        );
        let maximized = settings.get_bool("win.max", false);
        let hwnd = match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!("SpaceMaster"),
            WS_OVERLAPPEDWINDOW,
            x,
            y,
            ww.max(760),
            hh.max(440),
            None,
            None,
            Some(hinst.into()),
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                shell::error_box("SpaceMaster", &format!("Could not create the window: {e}"));
                return 1;
            }
        };
        let dpi = GetDpiForWindow(hwnd).max(96) as f32;
        let gfx = match Gfx::new(hwnd, dpi) {
            Ok(g) => g,
            Err(e) => {
                shell::error_box("SpaceMaster", &format!("Direct2D is not available: {e}"));
                return 1;
            }
        };
        let model = spacemaster::model::Model::new();
        if std::env::var_os("SM_NO_DRIVE_SCAN").is_some() {
            model.scan_drive.store(false, Ordering::Relaxed);
        }
        let browser = Browser::new(model, settings);
        let boxed_app = Box::new(App {
            hwnd,
            gfx,
            theme: Theme::current(),
            browser,
            cols: [ColView::default(); 3],
            hover: None,
            hover_row: None,
            mouse: (0.0, 0.0),
            hits: Vec::new(),
            side: Vec::new(),
            path_segs: Vec::new(),
            last_cwd: String::new(),
            last_cursor: None,
            last_preview: None,
            spinner: 0.0,
            help: false,
            banner_dismissed: false,
            elevated: shell::is_elevated(),
            viewer: viewer::Viewer::new(),
            fs_saved: None,
            timer_on: false,
            tracking: false,
            wheel: [0.0; 3],
            high_surrogate: None,
            thumbs: std::collections::HashMap::new(),
            thumb_order: Vec::new(),
            thumb_requested: None,
            thumb_worker: thumbs::Worker::start(hwnd),
        });
        APP_PTR.store(Box::into_raw(boxed_app), Ordering::SeqCst);
        let a = app().unwrap();
        if let Some(z) = a.browser.settings.get("zoom").and_then(|v| v.parse::<f32>().ok()) {
            a.gfx.set_zoom(z);
        }
        a.apply_theme();
        a.update_title();
        a.sync_view();
        let _ = ShowWindow(hwnd, if maximized { SW_SHOWMAXIMIZED } else { SW_SHOW });
        a.ensure_timer();
        pump_events();
        if let Some(out) = selftest_out {
            return selftest::run(hwnd, &out);
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        msg.wParam.0 as i32
    }
}

/// Runs the controller's queued events. Dialogs and shell calls happen here, *outside* any borrow of the app.
fn pump_events() {
    loop {
        let (hwnd, events) = match app() {
            Some(a) => (a.hwnd, std::mem::take(&mut a.browser.events)),
            None => return,
        };
        if events.is_empty() {
            break;
        }
        for ev in events {
            match ev {
                Event::Open(p) => shell::open(hwnd, &p),
                Event::Reveal(p) => shell::reveal(&p),
                Event::CopyText(t) => shell::set_clipboard_text(hwnd, &t),
                Event::CopyFiles(f) => shell::set_clipboard_files(hwnd, &f),
                Event::OpenRecycleBin => shell::open_recycle_bin(hwnd),
                Event::OpenViewer(p) => {
                    if let Some(a) = app() {
                        a.open_viewer(&p);
                    }
                }
                Event::ShowHelp => {
                    if let Some(a) = app() {
                        a.help = true;
                        a.invalidate();
                    }
                }
                Event::Alert { title, text } => shell::alert(hwnd, &title, &text),
                Event::ConfirmDelete(req) => {
                    let what = if req.items.len() == 1 { format!("\u{201c}{}\u{201d}", req.items[0].name) } else { format!("{} items", req.items.len()) };
                    let text = format!("This frees {} immediately and can't be undone.", format::bytes(req.bytes));
                    if shell::confirm(hwnd, &format!("Delete {what} permanently?"), &text) {
                        if let Some(a) = app() {
                            a.browser.start_delete(req);
                        }
                    }
                }
                Event::RunDelete(req) => spawn_delete(hwnd, req),
                Event::LocationChanged => {
                    if let Some(a) = app() {
                        a.update_title();
                    }
                }
            }
        }
    }
    if let Some(a) = app() {
        a.after_input();
    }
}

fn spawn_delete(hwnd: HWND, req: DeleteRequest) {
    let h = hwnd.0 as isize;
    std::thread::spawn(move || {
        let outcome = run_delete(&req, h);
        let boxed = Box::new((req, outcome));
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), WM_DELETED, WPARAM(0), LPARAM(Box::into_raw(boxed) as isize));
        }
    });
}

fn client_xy(lparam: LPARAM, scale: f32) -> (f32, f32) {
    let x = (lparam.0 & 0xFFFF) as i16 as f32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;
    (x / scale, y / scale)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(a) = app() else { return DefWindowProcW(hwnd, msg, wparam, lparam) };
    let scale = a.gfx.dpi / 96.0;
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(hwnd, &mut ps);
            a.paint();
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SIZE => {
            let (w, h) = ((lparam.0 & 0xFFFF) as u32, ((lparam.0 >> 16) & 0xFFFF) as u32);
            if w > 0 && h > 0 {
                a.gfx.resize(w, h);
                a.sync_view();
                a.invalidate();
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let dpi = (wparam.0 & 0xFFFF) as f32;
            a.gfx.set_dpi(dpi);
            let r = &*(lparam.0 as *const RECT);
            let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            a.invalidate();
            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            let mmi = &mut *(lparam.0 as *mut MINMAXINFO);
            let bs = a.gfx.base_scale();
            mmi.ptMinTrackSize = POINT { x: (760.0 * bs) as i32, y: (440.0 * bs) as i32 };
            LRESULT(0)
        }
        WM_TIMER => {
            a.timer();
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = client_xy(lparam, scale);
            if a.viewer.active {
                if a.viewer.mouse_move(&a.gfx, x, y) {
                    a.invalidate();
                }
                return LRESULT(0);
            }
            a.mouse_move(x, y);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if a.viewer.active {
                a.viewer.mouse_up();
                let _ = windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture();
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            a.mouse_leave();
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            let (x, y) = client_xy(lparam, scale);
            if a.viewer.active {
                if a.viewer.mouse_down(&a.gfx, x, y) {
                    windows::Win32::UI::Input::KeyboardAndMouse::SetCapture(hwnd);
                }
                a.invalidate();
                return LRESULT(0);
            }
            a.click(x, y, msg == WM_LBUTTONDBLCLK);
            pump_events();
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let (x, y) = client_xy(lparam, scale);
            if a.viewer.active {
                return LRESULT(0);
            }
            a.context_menu(x, y);
            pump_events();
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let mut pt = POINT { x: (lparam.0 & 0xFFFF) as i16 as i32, y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32 };
            let _ = ScreenToClient(hwnd, &mut pt);
            if a.viewer.active {
                a.viewer.wheel(delta, key_down(VK_CONTROL.0 as i32));
                a.invalidate();
            } else if key_down(VK_CONTROL.0 as i32) {
                a.change_zoom(if delta > 0 { 1 } else { -1 });
            } else {
                a.wheel(pt.x as f32 / scale, pt.y as f32 / scale, delta);
            }
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let handled = a.key_down(wparam.0 as u16, (lparam.0 >> 30) & 1 == 1);
            pump_events();
            if handled { LRESULT(0) } else { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_CHAR => {
            a.char_input(wparam.0 as u16, (lparam.0 >> 30) & 1 == 1);
            pump_events();
            LRESULT(0)
        }
        WM_SETFOCUS => LRESULT(0),
        WM_ACTIVATE => {
            if (wparam.0 & 0xFFFF) != 0 && a.browser.deleting == 0 && !a.viewer.active {
                a.browser.app_activated();
                a.after_input();
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_SETTINGCHANGE => {
            a.apply_theme();
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        viewer::WM_VIEWER => {
            let l = Box::from_raw(lparam.0 as *mut viewer::Loaded);
            let (dark, bg) = (a.theme.dark, a.theme.window);
            a.viewer.loaded(hwnd.0 as isize, *l, dark, bg);
            a.invalidate();
            a.ensure_timer();
            LRESULT(0)
        }
        thumbs::WM_THUMB => {
            let r = Box::from_raw(lparam.0 as *mut thumbs::Result);
            a.thumb_arrived(*r);
            LRESULT(0)
        }
        WM_DELETED => {
            let boxed = Box::from_raw(lparam.0 as *mut (DeleteRequest, DeleteOutcome));
            let (req, outcome) = *boxed;
            a.browser.finish_delete(req, outcome);
            a.after_input();
            pump_events();
            LRESULT(0)
        }
        WM_CLOSE => {
            a.save_placement();
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
