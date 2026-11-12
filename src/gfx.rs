//! Direct2D / DirectWrite drawing layer: a hardware accelerated HWND render target, cached text layouts and
//! cached shell icons. Everything is in DIPs; the render target carries the window DPI.

use std::collections::HashMap;
use windows::core::{w, Interface, PCWSTR};
use windows_numerics::Vector2;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_UNKNOWN;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_SMALLICON, SHGFI_USEFILEATTRIBUTES};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetClientRect};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const fn rgb(hex: u32) -> Color {
        Color { r: ((hex >> 16) & 255) as f32 / 255.0, g: ((hex >> 8) & 255) as f32 / 255.0, b: (hex & 255) as f32 / 255.0, a: 1.0 }
    }
    pub const fn rgba(hex: u32, a: f32) -> Color {
        let c = Color::rgb(hex);
        Color { r: c.r, g: c.g, b: c.b, a }
    }
    pub fn alpha(self, a: f32) -> Color {
        Color { a: self.a * a, ..self }
    }
    pub fn mix(self, other: Color, t: f32) -> Color {
        Color {
            r: self.r + (other.r - self.r) * t,
            g: self.g + (other.g - self.g) * t,
            b: self.b + (other.b - self.b) * t,
            a: self.a + (other.a - self.a) * t,
        }
    }
    /// HSV (h, s, v all 0..1) to RGB.
    pub fn hsv(h: f32, s: f32, v: f32) -> Color {
        let i = (h * 6.0).floor();
        let f = h * 6.0 - i;
        let (p, q, t) = (v * (1.0 - s), v * (1.0 - f * s), v * (1.0 - (1.0 - f) * s));
        let (r, g, b) = match (i as i32).rem_euclid(6) {
            0 => (v, t, p),
            1 => (q, v, p),
            2 => (p, v, t),
            3 => (p, q, v),
            4 => (t, p, v),
            _ => (v, p, q),
        };
        Color { r, g, b, a: 1.0 }
    }
    fn d2d(self) -> D2D1_COLOR_F {
        D2D1_COLOR_F { r: self.r, g: self.g, b: self.b, a: self.a }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Face {
    Name,
    Meta,
    Small,
    Badge,
    Header,
    Big,
    Glyph,
    GlyphSmall,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Align {
    Left,
    Right,
    Center,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
    pub fn inset(&self, d: f32) -> Rect {
        Rect::new(self.x + d, self.y + d, (self.w - 2.0 * d).max(0.0), (self.h - 2.0 * d).max(0.0))
    }
    fn d2d(&self) -> D2D_RECT_F {
        D2D_RECT_F { left: self.x, top: self.y, right: self.x + self.w, bottom: self.y + self.h }
    }
}

type LayoutKey = (String, Face, i32, Align);

pub struct Gfx {
    factory: ID2D1Factory,
    dwrite: IDWriteFactory,
    wic: Option<IWICImagingFactory>,
    hwnd: HWND,
    rt: Option<ID2D1HwndRenderTarget>,
    brush: Option<ID2D1SolidColorBrush>,
    formats: HashMap<(Face, Align), IDWriteTextFormat>,
    ellipsis: HashMap<Face, IDWriteInlineObject>,
    layouts: HashMap<LayoutKey, (IDWriteTextLayout, f32, f32)>,
    icons: HashMap<String, Option<ID2D1Bitmap>>,
    stops: HashMap<(u32, bool), ID2D1GradientStopCollection>,
    para: HashMap<Face, IDWriteTextFormat>,
    stroke: Option<ID2D1StrokeStyle>,
    pub dpi: f32,
    base_dpi: f32,
    zoom: f32,
    px: (u32, u32),
    pub width: f32,
    pub height: f32,
    glyph_font: &'static str,
}

impl Gfx {
    pub fn new(hwnd: HWND, dpi: f32) -> windows::core::Result<Gfx> {
        let factory: ID2D1Factory = unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        let wic: Option<IWICImagingFactory> =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok() };
        let mut g = Gfx {
            factory,
            dwrite,
            wic,
            hwnd,
            rt: None,
            brush: None,
            formats: HashMap::new(),
            ellipsis: HashMap::new(),
            layouts: HashMap::new(),
            icons: HashMap::new(),
            stops: HashMap::new(),
            para: HashMap::new(),
            stroke: None,
            dpi,
            base_dpi: dpi,
            zoom: 1.0,
            px: (1, 1),
            width: 0.0,
            height: 0.0,
            glyph_font: "Segoe Fluent Icons",
        };
        g.glyph_font = if g.font_exists("Segoe Fluent Icons") { "Segoe Fluent Icons" } else { "Segoe MDL2 Assets" };
        g.create_target()?;
        Ok(g)
    }

    fn font_exists(&self, name: &str) -> bool {
        unsafe {
            let mut coll: Option<IDWriteFontCollection> = None;
            if self.dwrite.GetSystemFontCollection(&mut coll, false).is_err() {
                return false;
            }
            let Some(coll) = coll else { return false };
            let w: Vec<u16> = name.encode_utf16().chain([0]).collect();
            let (mut idx, mut exists) = (0u32, windows::core::BOOL(0));
            coll.FindFamilyName(PCWSTR(w.as_ptr()), &mut idx, &mut exists).is_ok() && exists.as_bool()
        }
    }

    fn create_target(&mut self) -> windows::core::Result<()> {
        let mut rc = RECT::default();
        unsafe { GetClientRect(self.hwnd, &mut rc)? };
        let (pw, ph) = (((rc.right - rc.left).max(1)) as u32, ((rc.bottom - rc.top).max(1)) as u32);
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_UNKNOWN, alphaMode: D2D1_ALPHA_MODE_IGNORE },
            dpiX: 96.0,
            dpiY: 96.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let hprops = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd: self.hwnd,
            pixelSize: D2D_SIZE_U { width: pw, height: ph },
            presentOptions: D2D1_PRESENT_OPTIONS_IMMEDIATELY,
        };
        let rt = unsafe { self.factory.CreateHwndRenderTarget(&props, &hprops)? };
        unsafe {
            rt.SetDpi(self.dpi, self.dpi);
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE);
        }
        let brush = unsafe { rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }, None)? };
        self.brush = Some(brush);
        self.rt = Some(rt);
        self.icons.clear();
        self.stops.clear();
        self.stroke = unsafe {
            self.factory
                .CreateStrokeStyle(
                    &D2D1_STROKE_STYLE_PROPERTIES {
                        startCap: D2D1_CAP_STYLE_ROUND,
                        endCap: D2D1_CAP_STYLE_ROUND,
                        dashCap: D2D1_CAP_STYLE_ROUND,
                        lineJoin: D2D1_LINE_JOIN_ROUND,
                        miterLimit: 1.0,
                        dashStyle: D2D1_DASH_STYLE_SOLID,
                        dashOffset: 0.0,
                    },
                    None,
                )
                .ok()
        };
        self.px = (pw, ph);
        self.width = pw as f32 * 96.0 / self.dpi;
        self.height = ph as f32 * 96.0 / self.dpi;
        Ok(())
    }

    /// The monitor's DPI (changes when the window moves between monitors).
    pub fn set_dpi(&mut self, dpi: f32) {
        self.base_dpi = dpi;
        self.apply_dpi();
    }

    /// User zoom (Ctrl +/-): scales the whole interface, fonts included, and re-renders it crisply.
    pub fn set_zoom(&mut self, zoom: f32) {
        self.zoom = zoom.clamp(0.6, 2.5);
        self.apply_dpi();
    }

    /// Monitor scale without the user zoom (for window size limits).
    pub fn base_scale(&self) -> f32 {
        self.base_dpi / 96.0
    }

    fn apply_dpi(&mut self) {
        let eff = self.base_dpi * self.zoom;
        if (eff - self.dpi).abs() > 0.01 {
            self.dpi = eff;
            self.layouts.clear();
            self.icons.clear();
            if let Some(rt) = &self.rt {
                unsafe { rt.SetDpi(eff, eff) };
            }
            self.width = self.px.0.max(1) as f32 * 96.0 / eff;
            self.height = self.px.1.max(1) as f32 * 96.0 / eff;
        }
    }

    pub fn resize(&mut self, pw: u32, ph: u32) {
        if let Some(rt) = &self.rt {
            let _ = unsafe { rt.Resize(&D2D_SIZE_U { width: pw.max(1), height: ph.max(1) }) };
        }
        self.px = (pw.max(1), ph.max(1));
        self.width = pw.max(1) as f32 * 96.0 / self.dpi;
        self.height = ph.max(1) as f32 * 96.0 / self.dpi;
    }

    pub fn begin(&mut self) -> bool {
        if self.rt.is_none() && self.create_target().is_err() {
            return false;
        }
        unsafe { self.rt.as_ref().unwrap().BeginDraw() };
        true
    }

    /// Returns false when the device was lost and the target was dropped (draw again on the next paint).
    pub fn end(&mut self) -> bool {
        let r = unsafe { self.rt.as_ref().unwrap().EndDraw(None, None) };
        if r.is_err() {
            self.rt = None;
            self.brush = None;
            self.icons.clear();
            self.stops.clear();
            return false;
        }
        true
    }

    fn rt(&self) -> &ID2D1HwndRenderTarget {
        self.rt.as_ref().unwrap()
    }

    fn brush(&self, c: Color) -> &ID2D1SolidColorBrush {
        let b = self.brush.as_ref().unwrap();
        unsafe { b.SetColor(&c.d2d()) };
        b
    }

    // MARK: Primitives

    pub fn clear(&self, c: Color) {
        unsafe { self.rt().Clear(Some(&c.d2d())) };
    }

    pub fn fill(&self, r: Rect, c: Color) {
        unsafe { self.rt().FillRectangle(&r.d2d(), self.brush(c)) };
    }

    pub fn fill_round(&self, r: Rect, radius: f32, c: Color) {
        let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
        unsafe { self.rt().FillRoundedRectangle(&rr, self.brush(c)) };
    }

    pub fn stroke_round(&self, r: Rect, radius: f32, c: Color, width: f32) {
        let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
        unsafe { self.rt().DrawRoundedRectangle(&rr, self.brush(c), width, None) };
    }

    pub fn hline(&self, x0: f32, x1: f32, y: f32, c: Color) {
        self.fill(Rect::new(x0, y, x1 - x0, 1.0 * 96.0 / self.dpi.max(96.0)), c);
    }

    pub fn vline(&self, x: f32, y0: f32, y1: f32, c: Color) {
        self.fill(Rect::new(x, y0, 1.0 * 96.0 / self.dpi.max(96.0), y1 - y0), c);
    }

    pub fn push_clip(&self, r: Rect) {
        unsafe { self.rt().PushAxisAlignedClip(&r.d2d(), D2D1_ANTIALIAS_MODE_ALIASED) };
    }

    pub fn pop_clip(&self) {
        unsafe { self.rt().PopAxisAlignedClip() };
    }

    /// Horizontal gradient bar with rounded corners: `c0` at the left to `c1` at the right.
    pub fn gradient_bar(&mut self, r: Rect, radius: f32, bucket: u32, dark: bool, c0: Color, c1: Color) {
        let key = (bucket, dark);
        if !self.stops.contains_key(&key) {
            let st = [
                D2D1_GRADIENT_STOP { position: 0.0, color: c0.d2d() },
                D2D1_GRADIENT_STOP { position: 1.0, color: c1.d2d() },
            ];
            let Ok(coll) = (unsafe { self.rt().CreateGradientStopCollection(&st, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) })
            else {
                self.fill_round(r, radius, c0.mix(c1, 0.4));
                return;
            };
            self.stops.insert(key, coll);
        }
        let coll = &self.stops[&key];
        let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
            startPoint: Vector2::new(r.x, 0.0),
            endPoint: Vector2::new(r.right(), 0.0),
        };
        if let Ok(brush) = unsafe { self.rt().CreateLinearGradientBrush(&props, None, coll) } {
            let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
            unsafe { self.rt().FillRoundedRectangle(&rr, &brush) };
        }
    }

    // MARK: Text

    fn face_spec(&self, face: Face) -> (&'static str, f32, DWRITE_FONT_WEIGHT) {
        match face {
            Face::Name => ("Segoe UI Variable Text", 13.0, DWRITE_FONT_WEIGHT_MEDIUM),
            Face::Meta => ("Segoe UI Variable Text", 12.0, DWRITE_FONT_WEIGHT_NORMAL),
            Face::Small => ("Segoe UI Variable Text", 11.0, DWRITE_FONT_WEIGHT_NORMAL),
            Face::Badge => ("Segoe UI Variable Text", 10.5, DWRITE_FONT_WEIGHT_SEMI_BOLD),
            Face::Header => ("Segoe UI Variable Text", 11.5, DWRITE_FONT_WEIGHT_SEMI_BOLD),
            Face::Big => ("Segoe UI Variable Display", 18.0, DWRITE_FONT_WEIGHT_SEMI_BOLD),
            Face::Glyph => (self.glyph_font, 13.0, DWRITE_FONT_WEIGHT_NORMAL),
            Face::GlyphSmall => (self.glyph_font, 9.0, DWRITE_FONT_WEIGHT_NORMAL),
        }
    }

    fn format(&mut self, face: Face, align: Align) -> Option<IDWriteTextFormat> {
        if let Some(f) = self.formats.get(&(face, align)) {
            return Some(f.clone());
        }
        let (family, size, weight) = self.face_spec(face);
        let fam: Vec<u16> = family.encode_utf16().chain([0]).collect();
        let f = unsafe {
            self.dwrite
                .CreateTextFormat(
                    PCWSTR(fam.as_ptr()),
                    None,
                    weight,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    w!("en-us"),
                )
                .ok()?
        };
        unsafe {
            let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);
            let _ = f.SetTextAlignment(match align {
                Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
            });
            if !self.ellipsis.contains_key(&face) {
                if let Ok(sign) = self.dwrite.CreateEllipsisTrimmingSign(&f) {
                    self.ellipsis.insert(face, sign);
                }
            }
            if let Some(sign) = self.ellipsis.get(&face) {
                let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
                let _ = f.SetTrimming(&trim, sign);
            }
        }
        self.formats.insert((face, align), f.clone());
        Some(f)
    }

    const ROW: f32 = 24.0;

    fn layout(&mut self, s: &str, face: Face, width: f32, height: f32, align: Align) -> Option<&(IDWriteTextLayout, f32, f32)> {
        let key: LayoutKey = (s.to_string(), face, (width * 4.0) as i32 ^ ((height * 4.0) as i32) << 20, align);
        if !self.layouts.contains_key(&key) {
            if self.layouts.len() > 6000 {
                self.layouts.clear();
            }
            let fmt = self.format(face, align)?;
            let w: Vec<u16> = s.encode_utf16().collect();
            let layout = unsafe { self.dwrite.CreateTextLayout(&w, &fmt, width, height).ok()? };
            let mut m = DWRITE_TEXT_METRICS::default();
            unsafe {
                let _ = layout.GetMetrics(&mut m);
            }
            self.layouts.insert(key.clone(), (layout, m.widthIncludingTrailingWhitespace, m.height));
        }
        self.layouts.get(&key)
    }

    /// Rounds a DIP coordinate to a whole device pixel.
    pub fn snap(&self, v: f32) -> f32 {
        let s = self.dpi / 96.0;
        (v * s).round() / s
    }

    /// Draws a bitmap at its native pixel size (never rescaled), centred in `r` on the pixel grid.
    pub fn draw_icon(&self, bmp: &ID2D1Bitmap, r: Rect, opacity: f32) {
        let s = self.dpi / 96.0;
        let px = unsafe { bmp.GetPixelSize() };
        let (w, h) = (px.width as f32 / s, px.height as f32 / s);
        let dest = Rect::new(self.snap(r.x + (r.w - w) / 2.0), self.snap(r.y + (r.h - h) / 2.0), w, h);
        unsafe {
            self.rt().DrawBitmap(bmp, Some(&dest.d2d()), opacity, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None);
        }
    }

    /// Natural width of a single line of text.
    pub fn measure(&mut self, s: &str, face: Face) -> f32 {
        self.layout(s, face, 4000.0, Self::ROW, Align::Left).map(|l| l.1).unwrap_or(0.0)
    }

    /// Draws one line of text vertically centred in `r`; clipped with an ellipsis at the rect's width.
    pub fn text(&mut self, s: &str, face: Face, r: Rect, align: Align, c: Color) {
        if s.is_empty() || r.w <= 1.0 {
            return;
        }
        let Some((layout, lh)) = self.layout(s, face, r.w, r.h, align).map(|l| (l.0.clone(), l.2)) else { return };
        // Vertical centring and the origin are snapped to whole device pixels: text on a fractional position is
        // resampled by the rasterizer and looks soft.
        let (x, y) = (self.snap(r.x), self.snap(r.y + (r.h - lh) / 2.0));
        unsafe {
            self.rt().DrawTextLayout(
                Vector2::new(x, y),
                &layout,
                self.brush(c),
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
    }

    // MARK: Paragraphs and arcs

    fn para_layout(&mut self, s: &str, face: Face, width: f32, centered: bool) -> Option<IDWriteTextLayout> {
        if !self.para.contains_key(&face) {
            let (family, size, weight) = self.face_spec(face);
            let fam: Vec<u16> = family.encode_utf16().chain([0]).collect();
            let f = unsafe {
                self.dwrite
                    .CreateTextFormat(PCWSTR(fam.as_ptr()), None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("en-us"))
                    .ok()?
            };
            unsafe {
                let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
                let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);
            }
            self.para.insert(face, f);
        }
        let fmt = self.para.get(&face)?;
        let w: Vec<u16> = s.encode_utf16().collect();
        let layout = unsafe { self.dwrite.CreateTextLayout(&w, fmt, width.max(10.0), 4000.0).ok()? };
        if centered {
            unsafe {
                let _ = layout.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
            }
        }
        Some(layout)
    }

    pub fn paragraph_height(&mut self, s: &str, face: Face, width: f32) -> f32 {
        let Some(l) = self.para_layout(s, face, width, false) else { return 0.0 };
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe {
            let _ = l.GetMetrics(&mut m);
        }
        m.height
    }

    /// Word wrapped text at the top-left of `r`; returns the height used.
    pub fn paragraph(&mut self, s: &str, face: Face, r: Rect, c: Color) -> f32 {
        self.paragraph_impl(s, face, r, c, false)
    }

    pub fn paragraph_centered(&mut self, s: &str, face: Face, r: Rect, c: Color) -> f32 {
        self.paragraph_impl(s, face, r, c, true)
    }

    fn paragraph_impl(&mut self, s: &str, face: Face, r: Rect, c: Color, centered: bool) -> f32 {
        let Some(l) = self.para_layout(s, face, r.w, centered) else { return 0.0 };
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe {
            let _ = l.GetMetrics(&mut m);
            self.rt().DrawTextLayout(Vector2::new(self.snap(r.x), self.snap(r.y)), &l, self.brush(c), D2D1_DRAW_TEXT_OPTIONS_NONE);
        }
        m.height
    }

    /// Stroked arc (the progress spinner). Angles in degrees, clockwise on screen.
    pub fn arc(&self, center: (f32, f32), radius: f32, start_deg: f32, sweep_deg: f32, c: Color, width: f32) {
        let pt = |deg: f32| {
            let a = deg.to_radians();
            Vector2::new(center.0 + radius * a.cos(), center.1 + radius * a.sin())
        };
        unsafe {
            let Ok(geom) = self.factory.CreatePathGeometry() else { return };
            let Ok(sink) = geom.Open() else { return };
            sink.BeginFigure(pt(start_deg), D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddArc(&D2D1_ARC_SEGMENT {
                point: pt(start_deg + sweep_deg),
                size: D2D_SIZE_F { width: radius, height: radius },
                rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: if sweep_deg > 180.0 { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
            });
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            if sink.Close().is_ok() {
                self.rt().DrawGeometry(&geom, self.brush(c), width, self.stroke.as_ref());
            }
        }
    }

    // MARK: Icons

    /// Shell icon for a file extension ("" = generic file, "<dir>" = folder), cached.
    pub fn icon(&mut self, key: &str) -> Option<ID2D1Bitmap> {
        if let Some(i) = self.icons.get(key) {
            return i.clone();
        }
        if self.icons.len() > 2000 {
            self.icons.clear();
        }
        let (large, k) = match key.strip_prefix("L:") {
            Some(k) => (true, k),
            None => (false, key),
        };
        let bmp = self.load_icon(k, large);
        self.icons.insert(key.to_string(), bmp.clone());
        bmp
    }

    pub fn icon_large(&mut self, key: &str) -> Option<ID2D1Bitmap> {
        self.icon(&format!("L:{key}"))
    }

    fn load_icon(&self, key: &str, large: bool) -> Option<ID2D1Bitmap> {
        let wic = self.wic.as_ref()?;
        let (name, attrs) = if key == "<dir>" {
            ("folder".to_string(), 0x10u32)
        } else if key.is_empty() {
            ("file".to_string(), 0x80)
        } else {
            (format!("file.{key}"), 0x80)
        };
        let w: Vec<u16> = name.encode_utf16().chain([0]).collect();
        let mut info = SHFILEINFOW::default();
        let r = unsafe {
            SHGetFileInfoW(
                PCWSTR(w.as_ptr()),
                windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES(attrs),
                Some(&mut info),
                std::mem::size_of::<SHFILEINFOW>() as u32,
                SHGFI_ICON | if large { SHGFI_LARGEICON } else { SHGFI_SMALLICON } | SHGFI_USEFILEATTRIBUTES,
            )
        };
        if r == 0 || info.hIcon.is_invalid() {
            return None;
        }
        let out = unsafe {
            let bmp = wic.CreateBitmapFromHICON(info.hIcon).ok();
            let conv = bmp.and_then(|b| {
                let c = wic.CreateFormatConverter().ok()?;
                c.Initialize(&b, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeMedianCut)
                    .ok()?;
                Some(c)
            });
            let d2d = conv.and_then(|c| self.rt().CreateBitmapFromWicBitmap(&c.cast::<IWICBitmapSource>().ok()?, None).ok());
            let _ = DestroyIcon(info.hIcon);
            d2d
        };
        out
    }

}

impl Gfx {
    /// True while a render target exists (false after a lost device until the next paint recreates it).
    pub fn ready(&self) -> bool {
        self.rt.is_some()
    }
}

impl Gfx {
    pub fn zoom(&self) -> f32 {
        self.zoom
    }
}
