//! Colors for light and dark mode (follows the Windows app theme) and the system accent color.

use crate::gfx::Color;
use windows::core::w;
use windows::core::BOOL;
use windows::Win32::Graphics::Dwm::DwmGetColorizationColor;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

#[derive(Clone, Copy)]
pub struct Theme {
    pub dark: bool,
    pub window: Color,
    pub sidebar: Color,
    pub bar: Color,
    pub separator: Color,
    pub text: Color,
    pub text2: Color,
    pub text3: Color,
    pub accent: Color,
    pub hover: Color,
    pub field: Color,
    pub panel: Color,
    pub safe: Color,
    pub protected: Color,
    pub mixed: Color,
    /// Text drawn on top of the accent colour.
    pub on_accent: Color,
}

pub fn system_uses_dark() -> bool {
    unsafe {
        let mut v: u32 = 1;
        let mut sz = 4u32;
        let r = RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut u32 as *mut _),
            Some(&mut sz),
        );
        r.is_ok() && v == 0
    }
}

fn accent() -> Color {
    // The real system accent colour (Settings > Personalization > Colors), stored as 0xAABBGGRR.
    let mut v: u32 = 0;
    let mut sz = 4u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Accent"),
            w!("AccentColorMenu"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut u32 as *mut _),
            Some(&mut sz),
        )
    };
    if r.is_ok() {
        let col = Color::rgb(((v & 0xFF) << 16) | (v & 0xFF00) | ((v >> 16) & 0xFF));
        if (0.05..0.92).contains(&luminance(col)) {
            return col;
        }
    }
    let mut c = 0u32;
    let mut opaque = BOOL(0);
    if unsafe { DwmGetColorizationColor(&mut c, &mut opaque) }.is_ok() {
        let col = Color::rgb(c & 0x00FF_FFFF);
        if (0.12..0.85).contains(&luminance(col)) {
            return col;
        }
    }
    Color::rgb(0x0067C0)
}

pub fn luminance(c: Color) -> f32 {
    0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b
}

impl Theme {
    pub fn current() -> Theme {
        let dark = match std::env::var("SM_THEME").as_deref() {
            Ok("dark") => true,
            Ok("light") => false,
            _ => system_uses_dark(),
        };
        Self::new(dark, accent())
    }

    pub fn new(dark: bool, accent: Color) -> Theme {
        let on_accent = if luminance(accent) > 0.58 { Color::rgb(0x111111) } else { Color::rgb(0xFFFFFF) };
        if dark {
            Theme {
                dark,
                window: Color::rgb(0x1C1C1E),
                sidebar: Color::rgb(0x242426),
                bar: Color::rgb(0x2A2A2D),
                separator: Color::rgba(0xFFFFFF, 0.09),
                text: Color::rgb(0xF4F4F5),
                text2: Color::rgba(0xF4F4F5, 0.72),
                text3: Color::rgba(0xF4F4F5, 0.50),
                accent,
                hover: Color::rgba(0xFFFFFF, 0.07),
                field: Color::rgba(0xFFFFFF, 0.07),
                panel: Color::rgb(0x2E2E31),
                safe: Color::rgb(0x4CD27A),
                protected: Color::rgb(0xFF6B63),
                mixed: Color::rgb(0xFFA94D),
                on_accent,
            }
        } else {
            Theme {
                dark,
                window: Color::rgb(0xFAFAFA),
                sidebar: Color::rgb(0xF0F0F2),
                bar: Color::rgb(0xF3F3F5),
                separator: Color::rgba(0x000000, 0.10),
                text: Color::rgb(0x121214),
                text2: Color::rgba(0x121214, 0.72),
                text3: Color::rgba(0x121214, 0.52),
                accent,
                hover: Color::rgba(0x000000, 0.05),
                field: Color::rgba(0x000000, 0.055),
                panel: Color::rgb(0xFFFFFF),
                safe: Color::rgb(0x1E9E50),
                protected: Color::rgb(0xD73A31),
                mixed: Color::rgb(0xD97706),
                on_accent,
            }
        }
    }

    /// Bar colour for a share bucket (see `model::share_hue`).
    pub fn bar(&self, hue: f32) -> Color {
        if self.dark { Color::hsv(hue, 0.88, 0.60) } else { Color::hsv(hue, 0.72, 0.98) }
    }
}
