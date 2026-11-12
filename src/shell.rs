//! Windows shell integration: open, reveal in Explorer, clipboard, message boxes, elevation, local time.

use spacemaster::paths;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::Storage::FileSystem::FileTimeToLocalFileTime;
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE, GMEM_ZEROINIT};
use windows::Win32::System::Ole::{CF_HDROP, CF_UNICODETEXT};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::System::Time::FileTimeToSystemTime;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{ILFree, SHOpenFolderAndSelectItems, SHParseDisplayName, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDYES, MB_DEFBUTTON2, MB_ICONERROR, MB_ICONWARNING, MB_OK, MB_YESNO, SW_SHOWNORMAL,
};

pub fn open(hwnd: HWND, path: &str) {
    let p = paths::to_wide(path);
    unsafe {
        ShellExecuteW(Some(hwnd), w!("open"), PCWSTR(p.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

pub fn open_recycle_bin(hwnd: HWND) {
    unsafe {
        ShellExecuteW(Some(hwnd), w!("open"), w!("explorer.exe"), w!("shell:RecycleBinFolder"), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

/// Opens File Explorer with the items selected.
pub fn reveal(items: &[String]) {
    let Some(first) = items.first() else { return };
    let Some(parent) = paths::parent(first) else { return };
    unsafe {
        let mut folder: *mut ITEMIDLIST = std::ptr::null_mut();
        let pw = paths::to_wide(&parent);
        if SHParseDisplayName(PCWSTR(pw.as_ptr()), None, &mut folder, 0, None).is_err() || folder.is_null() {
            return;
        }
        let mut children: Vec<*mut ITEMIDLIST> = Vec::new();
        for it in items {
            let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
            let w = paths::to_wide(it);
            if SHParseDisplayName(PCWSTR(w.as_ptr()), None, &mut pidl, 0, None).is_ok() && !pidl.is_null() {
                children.push(pidl);
            }
        }
        // Select by absolute PIDLs: pass the item PIDLs with no parent folder.
        let abs: Vec<*const ITEMIDLIST> = children.iter().map(|p| *p as *const _).collect();
        if abs.is_empty() {
            let _ = SHOpenFolderAndSelectItems(folder, None, 0);
        } else {
            let _ = SHOpenFolderAndSelectItems(abs[0], None, 0);
            // (Explorer opens the containing folder for the first item; additional selection needs a common
            // parent PIDL, which the shell derives itself when given absolute PIDLs one by one.)
        }
        for p in children {
            ILFree(Some(p));
        }
        ILFree(Some(folder));
    }
}

pub fn set_clipboard_text(hwnd: HWND, text: &str) {
    let w: Vec<u16> = text.encode_utf16().chain([0]).collect();
    unsafe {
        if OpenClipboard(Some(hwnd)).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(h) = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, w.len() * 2) {
            let p = GlobalLock(h) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
                let _ = GlobalUnlock(h);
                let _ = SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(h.0)));
            }
        }
        let _ = CloseClipboard();
    }
}

/// Puts files on the clipboard (Explorer can paste them).
pub fn set_clipboard_files(hwnd: HWND, files: &[String]) {
    // DROPFILES { pFiles: u32, pt: POINT(i32,i32), fNC: BOOL(i32), fWide: BOOL(i32) } = 20 bytes, then the list.
    let mut list: Vec<u16> = Vec::new();
    for f in files {
        list.extend(f.encode_utf16());
        list.push(0);
    }
    list.push(0);
    let header = 20usize;
    let total = header + list.len() * 2;
    unsafe {
        if OpenClipboard(Some(hwnd)).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(h) = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, total) {
            let p = GlobalLock(h) as *mut u8;
            if !p.is_null() {
                *(p as *mut u32) = header as u32; // pFiles
                *(p.add(16) as *mut i32) = 1; // fWide
                std::ptr::copy_nonoverlapping(list.as_ptr() as *const u8, p.add(header), list.len() * 2);
                let _ = GlobalUnlock(h);
                let _ = SetClipboardData(CF_HDROP.0 as u32, Some(HANDLE(h.0)));
            }
        }
        let _ = CloseClipboard();
    }
}

pub fn get_clipboard_text(hwnd: HWND) -> Option<String> {
    unsafe {
        OpenClipboard(Some(hwnd)).ok()?;
        let mut out = None;
        if let Ok(h) = GetClipboardData(CF_UNICODETEXT.0 as u32) {
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u16;
            if !p.is_null() {
                let mut n = 0;
                while *p.add(n) != 0 {
                    n += 1;
                }
                out = Some(String::from_utf16_lossy(std::slice::from_raw_parts(p, n)));
                let _ = GlobalUnlock(g);
            }
        }
        let _ = CloseClipboard();
        out
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

pub fn alert(hwnd: HWND, title: &str, text: &str) {
    if auto_dialogs() {
        LAST_ALERT.lock().unwrap().replace(format!("{title}: {text}"));
        return;
    }
    let (t, x) = (wide(title), wide(text));
    unsafe {
        MessageBoxW(Some(hwnd), PCWSTR(x.as_ptr()), PCWSTR(t.as_ptr()), MB_OK | MB_ICONWARNING);
    }
}

pub fn error_box(title: &str, text: &str) {
    let (t, x) = (wide(title), wide(text));
    unsafe {
        MessageBoxW(None, PCWSTR(x.as_ptr()), PCWSTR(t.as_ptr()), MB_OK | MB_ICONERROR);
    }
}

/// Yes/No question; "No" is the default button.
pub fn confirm(hwnd: HWND, title: &str, text: &str) -> bool {
    if auto_dialogs() {
        LAST_ALERT.lock().unwrap().replace(format!("{title}: {text}"));
        return true;
    }
    let (t, x) = (wide(title), wide(text));
    unsafe { MessageBoxW(Some(hwnd), PCWSTR(x.as_ptr()), PCWSTR(t.as_ptr()), MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2) == IDYES }
}

pub fn is_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut e = TOKEN_ELEVATION::default();
        let mut ret = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut e as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret,
        )
        .is_ok();
        let _ = windows::Win32::Foundation::CloseHandle(token);
        ok && e.TokenIsElevated != 0
    }
}

/// Starts a new, elevated instance of this program. Returns true when the user accepted the UAC prompt.
pub fn relaunch_as_admin(hwnd: HWND) -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let p = wide(&exe.to_string_lossy());
    let r = unsafe { ShellExecuteW(Some(hwnd), w!("runas"), PCWSTR(p.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
    r.0 as isize > 32
}

/// "2024-02-29 14:05" in local time.
pub fn format_local_time(filetime: i64) -> String {
    if filetime <= 0 {
        return String::new();
    }
    unsafe {
        let ft = windows::Win32::Foundation::FILETIME { dwLowDateTime: filetime as u32, dwHighDateTime: (filetime >> 32) as u32 };
        let mut local = windows::Win32::Foundation::FILETIME::default();
        if FileTimeToLocalFileTime(&ft, &mut local).is_err() {
            return String::new();
        }
        let mut st = windows::Win32::Foundation::SYSTEMTIME::default();
        if FileTimeToSystemTime(&local, &mut st).is_err() {
            return String::new();
        }
        format!("{:04}-{:02}-{:02} {:02}:{:02}", st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute)
    }
}

/// Selftest mode: dialogs answer themselves ("Yes" to confirmations) instead of blocking on the user.
pub static AUTO_DIALOGS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn auto_dialogs() -> bool {
    AUTO_DIALOGS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Last dialog text seen in selftest mode.
pub static LAST_ALERT: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
