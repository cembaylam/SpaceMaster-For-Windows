#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod gfx;
mod shell;
mod theme;

use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};

fn main() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    std::process::exit(app::run());
}
