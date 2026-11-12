# One-off patch script used while adding the PDF view to viewer.rs (kept for reference).
$f = "src/viewer.rs"
$t = [IO.File]::ReadAllText($f)
function Rep($old, $new) {
    if (-not $script:t.Contains($old)) { throw "missing: $old" }
    $script:t = $script:t.Replace($old, $new)
}

Rep 'use std::collections::HashMap;' @'
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
'@

Rep @'
    Media,
    Error(String),
}

pub struct Loaded {
'@ @'
    Media,
    Pdf,
    PdfOpened { count: usize, first: (f32, f32) },
    PdfPage { page: usize, req_w: u32, w: u32, h: u32, data: Vec<u8> },
    Error(String),
}

pub struct Loaded {
'@

Rep '        docs::Kind::Video | docs::Kind::Audio => return (Payload::Media, None),' @'
        docs::Kind::Video | docs::Kind::Audio => return (Payload::Media, None),
        docs::Kind::Pdf => return (Payload::Pdf, None),
'@

Rep @'
enum Content {
    Loading,
    Image(ImageView),
'@ @'
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
'@

Rep '    web: Option<WebView>,' @'
    web: Option<WebView>,
    pdf_tx: Option<Sender<PdfCmd>>,
    pdf_token: Arc<AtomicU32>,
'@

Rep '            web: None,' @'
            web: None,
            pdf_tx: None,
            pdf_token: Arc::new(AtomicU32::new(0)),
'@

Rep '        self.token = self.token.wrapping_add(1);
        self.active = true;' '        self.token = self.token.wrapping_add(1);
        self.pdf_token.store(self.token, Ordering::Relaxed);
        self.active = true;'

Rep '    pub fn close(&mut self) {
        self.token = self.token.wrapping_add(1);' '    pub fn close(&mut self) {
        self.token = self.token.wrapping_add(1);
        self.pdf_token.store(self.token, Ordering::Relaxed);'

Rep '            Payload::Media => self.start_media(hwnd),' @'
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
'@

Rep '            Content::Text(tv) => paint_text(g, t, area, tv),
            Content::Media | Content::Web => {}
        }
        g.pop_clip();' @'
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
'@

Rep '        g.push_clip(area);
        match &mut self.content {' '        g.push_clip(area);
        let mut pdf_reqs: Vec<(usize, u32)> = Vec::new();
        match &mut self.content {'

Rep '        if let (Content::Media, Some(p)) = (&self.content, &self.player) {
            return format!("vol' '        if let Content::Pdf(pv) = &self.content {
            return format!("page {} of {}  \u{b7}  {}", pv.cur_page + 1, pv.sizes.len(), self.caption);
        }
        if let (Content::Media, Some(p)) = (&self.content, &self.player) {
            return format!("vol'

Rep '            Content::Web => {
                if let Some(w) = &self.web {' @'
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
'@

Rep '            Content::Media => self.adjust_volume((notch * 5.0) as i32),' @'
            Content::Pdf(pv) => {
                if ctrl {
                    pv.zoom = (pv.zoom * 1.15f32.powf(notch)).clamp(0.3, 5.0);
                } else {
                    pv.scroll = (pv.scroll - notch * 96.0 * 3.0).clamp(0.0, pv.max_scroll);
                }
            }
            Content::Media => self.adjust_volume((notch * 5.0) as i32),
'@

Rep '            Content::Image(_) => "image",' '            Content::Image(_) => "image",
            Content::Pdf(_) => "pdf",'

$t += @'

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
'@

[IO.File]::WriteAllText($f, $t)
"patched"
