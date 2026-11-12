//! Shell thumbnails for the file info panel. One background worker asks the Windows shell for a thumbnail
//! (`IShellItemImageFactory`), so slow formats (video, HEIC, PDF) never block the UI. Only the newest request is
//! served: while the cursor races over files, outdated requests are dropped.

use spacemaster::paths;
use std::sync::{Arc, Condvar, Mutex};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, HGDIOBJ,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::UI::Shell::{IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK, SIIGBF_THUMBNAILONLY};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

pub const WM_THUMB: u32 = 0x8000 + 2; // WM_APP + 2

pub struct Request {
    pub key: String,
    pub path: String,
    pub w: u32,
    pub h: u32,
}

/// `w == 0`: the shell has no thumbnail for this file.
pub struct Result {
    pub key: String,
    pub w: u32,
    pub h: u32,
    /// Premultiplied BGRA, top-down.
    pub data: Vec<u8>,
}

pub struct Worker {
    slot: Arc<(Mutex<Option<Request>>, Condvar)>,
}

impl Worker {
    pub fn start(hwnd: HWND) -> Worker {
        let slot: Arc<(Mutex<Option<Request>>, Condvar)> = Arc::new((Mutex::new(None), Condvar::new()));
        let s = slot.clone();
        let h = hwnd.0 as isize;
        std::thread::Builder::new()
            .name("SpaceMaster.thumbs".into())
            .spawn(move || {
                unsafe {
                    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                }
                loop {
                    let req = {
                        let (m, cv) = &*s;
                        let mut g = m.lock().unwrap();
                        loop {
                            if let Some(r) = g.take() {
                                break r;
                            }
                            g = cv.wait(g).unwrap();
                        }
                    };
                    let res = fetch(&req).unwrap_or(Result { key: req.key.clone(), w: 0, h: 0, data: Vec::new() });
                    let boxed = Box::new(res);
                    unsafe {
                        if PostMessageW(Some(HWND(h as *mut _)), WM_THUMB, WPARAM(0), LPARAM(Box::into_raw(boxed) as isize)).is_err() {
                            return; // window is gone
                        }
                    }
                }
            })
            .expect("spawn thumbnail thread");
        Worker { slot }
    }

    /// Replaces any request that has not started yet.
    pub fn request(&self, r: Request) {
        let (m, cv) = &*self.slot;
        *m.lock().unwrap() = Some(r);
        cv.notify_one();
    }
}

fn fetch(req: &Request) -> Option<Result> {
    unsafe {
        let wide = paths::to_wide(&req.path);
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None).ok()?;
        let hbm = factory
            .GetImage(SIZE { cx: req.w as i32, cy: req.h as i32 }, SIIGBF_THUMBNAILONLY | SIIGBF_BIGGERSIZEOK)
            .ok()?;
        let mut bm = BITMAP::default();
        if GetObjectW(HGDIOBJ(hbm.0), std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _)) == 0 {
            let _ = DeleteObject(HGDIOBJ(hbm.0));
            return None;
        }
        let (w, h) = (bm.bmWidth as u32, bm.bmHeight as u32);
        let mut data = vec![0u8; (w * h * 4) as usize];
        let mut bi = BITMAPINFO::default();
        bi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32), // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: 0,
            ..Default::default()
        };
        let dc = GetDC(None);
        let lines = GetDIBits(dc, hbm, 0, h, Some(data.as_mut_ptr().cast()), &mut bi, DIB_RGB_COLORS);
        ReleaseDC(None, dc);
        let _ = DeleteObject(HGDIOBJ(hbm.0));
        if lines == 0 {
            return None;
        }
        // Bitmaps without an alpha channel come back with alpha 0: make them opaque.
        if data.chunks_exact(4).all(|p| p[3] == 0) {
            for p in data.chunks_exact_mut(4) {
                p[3] = 255;
            }
        }
        Some(Result { key: req.key.clone(), w, h, data })
    }
}
