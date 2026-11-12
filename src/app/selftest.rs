//! `spacemaster.exe --selftest <result-file>`: drives the real window through its real message handler
//! (paint, keyboard, mouse, wheel, resize, theme switch, deleting) and writes PASS/FAIL lines to the file.
//! Used by `tests/ui_smoke.rs`; dialogs answer themselves in this mode.

use super::*;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

struct Report {
    text: String,
    failures: i32,
}

impl Report {
    fn check(&mut self, name: &str, ok: bool, detail: impl FnOnce() -> String) {
        if ok {
            let _ = writeln!(self.text, "PASS {name}");
        } else {
            self.failures += 1;
            let _ = writeln!(self.text, "FAIL {name}: {}", detail());
        }
    }
}

fn send(hwnd: HWND, msg: u32, w: usize, l: isize) {
    unsafe {
        SendMessageW(hwnd, msg, Some(WPARAM(w)), Some(LPARAM(l)));
    }
}

fn key(hwnd: HWND, vk: VIRTUAL_KEY) {
    send(hwnd, WM_KEYDOWN, vk.0 as usize, 0);
}

fn chars(hwnd: HWND, s: &str) {
    for c in s.chars() {
        send(hwnd, WM_CHAR, c as usize, 0);
    }
}

fn mouse_lparam(x: f32, y: f32, scale: f32) -> isize {
    let (px, py) = ((x * scale) as i32 as u32 & 0xFFFF, (y * scale) as i32 as u32 & 0xFFFF);
    ((py << 16) | px) as isize
}

/// Runs the message queue (delete worker results arrive as posted messages) until `done` or timeout.
fn pump_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        if done() {
            return true;
        }
        if start.elapsed() > timeout {
            return false;
        }
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn paint_now(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
        let _ = windows::Win32::Graphics::Gdi::UpdateWindow(hwnd);
    }
}

pub fn run(hwnd: HWND, out: &str) -> i32 {
    shell::AUTO_DIALOGS.store(true, Ordering::Relaxed);
    let mut r = Report { text: String::new(), failures: 0 };
    run_checks(hwnd, &mut r);
    let _ = std::fs::write(out, &r.text);
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    r.failures
}

fn run_checks(hwnd: HWND, r: &mut Report) {
    let a = || app().expect("app");
    let scale = a().gfx.dpi / 96.0;

    // 1. Window, render target and first paint.
    paint_now(hwnd);
    r.check("window has a render target", a().gfx.ready(), || "no Direct2D target".into());
    r.check("layout has size", a().gfx.width > 700.0 && a().gfx.height > 400.0, || format!("{}x{}", a().gfx.width, a().gfx.height));

    // 2. Live scanning finishes and the list is sorted by size.
    let done = pump_until(Duration::from_secs(60), || {
        a().timer();
        !a().browser.is_busy()
    });
    r.check("scan finishes", done, || "scan timeout".into());
    a().browser.refresh_visible();
    paint_now(hwnd);
    let sizes: Vec<i64> = a().browser.current.borrow().rows.iter().map(|r| r.size).collect();
    r.check("rows sorted by size (desc)", sizes.windows(2).all(|w| w[0] >= w[1]) && sizes.len() >= 3, || format!("{sizes:?}"));
    let start_cwd = a().browser.cwd.clone();
    let title = {
        let mut buf = [0u16; 512];
        let n = unsafe { GetWindowTextW(hwnd, &mut buf) } as usize;
        String::from_utf16_lossy(&buf[..n])
    };
    r.check("window title shows the folder", title.starts_with(&start_cwd) && title.ends_with("SpaceMaster"), || title.clone());

    // 3. Keyboard through the real message handler.
    key(hwnd, VK_DOWN);
    r.check("Down moves the cursor", a().browser.cursor_index() == 1, || format!("{}", a().browser.cursor_index()));
    chars(hwnd, "j");
    r.check("j moves the cursor", a().browser.cursor_index() == 2, || format!("{}", a().browser.cursor_index()));
    chars(hwnd, "k");
    key(hwnd, VK_UP);
    r.check("k and Up move back", a().browser.cursor_index() == 0, || format!("{}", a().browser.cursor_index()));
    chars(hwnd, "G");
    let n = a().browser.current.borrow().rows.len();
    r.check("G jumps to the end", a().browser.cursor_index() == n - 1, || format!("{} of {n}", a().browser.cursor_index()));
    chars(hwnd, "gg");
    r.check("gg jumps to the top", a().browser.cursor_index() == 0, || format!("{}", a().browser.cursor_index()));
    chars(hwnd, ",n");
    r.check(",n sorts by name", a().browser.sort == SortKey::Name, || format!("{:?}", a().browser.sort));
    chars(hwnd, ",s");
    chars(hwnd, "v");
    r.check("v selects all", a().browser.selection.len() == n, || format!("{}", a().browser.selection.len()));
    key(hwnd, VK_ESCAPE);
    r.check("Esc clears the selection", a().browser.selection.is_empty(), || "still selected".into());

    // 4. Filter prompt.
    chars(hwnd, "/");
    chars(hwnd, "zzzzqq");
    r.check("filter prompt filters", a().browser.current.borrow().rows.is_empty() && a().browser.prompt.is_some(), || "no filtering".into());
    paint_now(hwnd);
    key(hwnd, VK_ESCAPE);
    r.check("Esc closes and clears the filter", a().browser.prompt.is_none() && a().browser.current.borrow().rows.len() == n, || "filter stuck".into());

    // 5. Help overlay.
    key(hwnd, VK_F1);
    r.check("F1 shows help", a().help, || "no help".into());
    paint_now(hwnd);
    key(hwnd, VK_ESCAPE);
    r.check("any key closes help", !a().help, || "help stuck".into());

    // 6. Mouse: click selects a row; double click on a folder enters it.
    let lay = a().layout();
    let list = lay.lists[1];
    send(hwnd, WM_LBUTTONDOWN, 0, mouse_lparam(list.x + 120.0, list.y + 2.5 * ROW_H, scale));
    r.check("click moves the cursor", a().browser.cursor_index() == 2, || format!("{}", a().browser.cursor_index()));
    // Find the first directory row and double-click it.
    let dir_idx = a().browser.current.borrow().rows.iter().position(|r| r.is_dir());
    if let Some(i) = dir_idx {
        let off = a().cols[1].offset;
        let y = list.y + ((i - off) as f32 + 0.5) * ROW_H;
        send(hwnd, WM_LBUTTONDBLCLK, 0, mouse_lparam(list.x + 120.0, y, scale));
        r.check("double click enters a folder", a().browser.cwd != start_cwd && a().browser.cwd.starts_with(&start_cwd), || a().browser.cwd.clone());
        key(hwnd, VK_BACK);
        r.check("Backspace goes up", a().browser.cwd == start_cwd, || a().browser.cwd.clone());
    }
    // Breadcrumb: click "This PC".
    paint_now(hwnd);
    let crumb = a().hits.iter().find(|(_, h)| *h == Hit::Path(0)).map(|(rc, _)| *rc);
    if let Some(rc) = crumb {
        send(hwnd, WM_LBUTTONDOWN, 0, mouse_lparam(rc.x + 6.0, rc.y + 6.0, scale));
        r.check("breadcrumb click opens This PC", a().browser.cwd.is_empty(), || a().browser.cwd.clone());
        r.check("This PC lists drives", !a().browser.current.borrow().rows.is_empty(), || "no drives".into());
        paint_now(hwnd);
    }
    // Sidebar: click a favourite.
    paint_now(hwnd);
    let side = a().hits.iter().find(|(_, h)| matches!(h, Hit::Side(1))).map(|(rc, _)| *rc);
    if let Some(rc) = side {
        send(hwnd, WM_LBUTTONDOWN, 0, mouse_lparam(rc.x + 20.0, rc.y + 8.0, scale));
        r.check("sidebar Home opens the home folder", a().browser.cwd == a().browser.model.home, || a().browser.cwd.clone());
    }
    chars(hwnd, "H");
    chars(hwnd, "H");
    chars(hwnd, "H");

    // 7. Resize and repaint at several sizes (render target resize path).
    for (w, h) in [(900, 560), (1600, 900), (1000, 700)] {
        unsafe {
            let _ = SetWindowPos(hwnd, None, 0, 0, w, h, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
        paint_now(hwnd);
        let ok = a().gfx.ready() && (a().gfx.width * scale - w as f32).abs() < 60.0 * scale;
        r.check(&format!("resize to {w}x{h} repaints"), ok, || format!("gfx {}x{}", a().gfx.width, a().gfx.height));
    }
    let vis = a().browser.visible_rows;
    r.check("visible rows follow the window height", vis >= 10, || format!("{vis}"));

    // 8. Wheel scrolling (only when the list is longer than the window).
    let lay = a().layout();
    let list = lay.lists[1];
    let rows = a().browser.current.borrow().rows.len();
    if rows > a().visible_rows(list) + 2 {
        let before = a().cols[1].offset;
        let pt = POINT { x: ((list.x + 100.0) * scale) as i32, y: ((list.y + 100.0) * scale) as i32 };
        let mut spt = pt;
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::ClientToScreen(hwnd, &mut spt);
        }
        let lp = ((spt.y as u32 & 0xFFFF) << 16 | (spt.x as u32 & 0xFFFF)) as isize;
        send(hwnd, WM_MOUSEWHEEL, ((-120i32 as u32 as usize) & 0xFFFF) << 16, lp);
        r.check("wheel scrolls the list", a().cols[1].offset > before, || format!("{} -> {}", before, a().cols[1].offset));
    }

    // 9. Theme switch.
    for dark in [true, false] {
        a().theme = Theme::new(dark, a().theme.accent);
        paint_now(hwnd);
        r.check(if dark { "dark theme paints" } else { "light theme paints" }, a().gfx.ready(), || "paint failed".into());
    }

    // 10. Deleting through the UI: create a file in a scratch folder, delete it with `D` (dialogs auto-confirm).
    let scratch = std::env::var("SM_START_PATH").ok();
    if let Some(dir) = scratch {
        let victim = format!("{dir}\\selftest-victim.bin");
        let _ = std::fs::write(&victim, vec![1u8; 200_000]);
        if a().browser.cd(&dir, None, true) {
            a().browser.app_activated();
            let idx = a().browser.current.borrow().index_of(Some("selftest-victim.bin"));
            r.check("created file is listed", idx.is_some(), || "missing".into());
            if let Some(i) = idx {
                a().browser.move_cursor(i as isize);
                chars(hwnd, "D");
                let gone = pump_until(Duration::from_secs(20), || !std::path::Path::new(&victim).exists() && a().browser.deleting == 0);
                r.check("D deletes the file after confirmation", gone, || "still there".into());
                let listed = a().browser.current.borrow().index_of(Some("selftest-victim.bin")).is_some();
                r.check("deleted file leaves the list", !listed, || "still listed".into());
                r.check("a confirmation was asked", shell::LAST_ALERT.lock().unwrap().is_some(), || "no dialog".into());
            }
            // Protected items are refused with an alert instead of being deleted.
            *shell::LAST_ALERT.lock().unwrap() = None;
            if a().browser.cd("C:\\Windows", None, true) {
                chars(hwnd, "D");
                let alerted = shell::LAST_ALERT.lock().unwrap().clone();
                r.check("protected item is refused", alerted.is_some_and(|t| t.contains("can't be deleted")), || "no refusal".into());
                r.check("no delete started for protected", a().browser.deleting == 0, || "delete running".into());
            }
        }
    }

    // 10b. Zoom (Ctrl +/-/0 and Ctrl+wheel call this): layout shrinks in DIPs, fonts re-render, reset restores.
    let w0 = a().gfx.width;
    a().change_zoom(1);
    a().change_zoom(1);
    paint_now(hwnd);
    r.check("zoom in scales the layout", a().gfx.zoom() > 1.15 && a().gfx.width < w0 * 0.9 && a().gfx.ready(), || format!("{} {}", a().gfx.zoom(), a().gfx.width));
    for _ in 0..30 {
        a().change_zoom(-1);
    }
    paint_now(hwnd);
    r.check("zoom out is limited to 60%", (a().gfx.zoom() - 0.6).abs() < 0.001 && a().gfx.ready(), || format!("{}", a().gfx.zoom()));
    a().change_zoom(0);
    paint_now(hwnd);
    r.check("zoom reset restores 100%", (a().gfx.zoom() - 1.0).abs() < 0.001 && (a().gfx.width - w0).abs() < 1.0, || format!("{} {}", a().gfx.zoom(), a().gfx.width));
    r.check("zoom is saved to settings", a().browser.settings.get("zoom") == Some("1"), || format!("{:?}", a().browser.settings.get("zoom")));

    // 10c. Thumbnail of an image file arrives from the background worker and is drawn.
    if let Ok(dir) = std::env::var("SM_START_PATH") {
        let pic = format!("{dir}\\selftest-pic.bmp");
        let (w, h) = (200u32, 120u32);
        let mut bmp: Vec<u8> = Vec::new();
        let size = 54 + w * h * 3;
        bmp.extend_from_slice(b"BM");
        bmp.extend_from_slice(&size.to_le_bytes());
        bmp.extend_from_slice(&[0, 0, 0, 0, 54, 0, 0, 0, 40, 0, 0, 0]);
        bmp.extend_from_slice(&w.to_le_bytes());
        bmp.extend_from_slice(&h.to_le_bytes());
        bmp.extend_from_slice(&[1, 0, 24, 0, 0, 0, 0, 0]);
        bmp.extend_from_slice(&(w * h * 3).to_le_bytes());
        bmp.extend_from_slice(&[0; 16]);
        for y in 0..h {
            for x in 0..w {
                bmp.extend_from_slice(&[(x * 255 / w) as u8, (y * 255 / h) as u8, 200]);
            }
        }
        let _ = std::fs::write(&pic, &bmp);
        if a().browser.cd(&dir, None, true) {
            a().browser.app_activated();
            let idx = a().browser.current.borrow().index_of(Some("selftest-pic.bmp"));
            if let Some(i) = idx {
                a().browser.move_cursor(i as isize);
                a().after_input();
                let got = pump_until(Duration::from_secs(20), || a().thumbs.values().any(|t| t.w > 0));
                r.check("thumbnail arrives for an image file", got, || "no thumbnail".into());
                paint_now(hwnd);
                let drawn = a().thumbs.values().any(|t| t.bmp.is_some());
                r.check("thumbnail is drawn in the info panel", drawn, || "no device bitmap".into());
            } else {
                r.check("image file is listed", false, || "missing".into());
            }
        }
        let _ = std::fs::remove_file(&pic);
    }

    // 10d. The viewer (Space): code, image, binary/hex, next/prev, close.
    if let Ok(dir) = std::env::var("SM_START_PATH") {
        let code: String = (0..400).map(|i| format!("fn line_{i}() {{ let x = {i}; }} // comment\n")).collect();
        let _ = std::fs::write(format!("{dir}\\viewer-code.rs"), &code);
        let _ = std::fs::write(format!("{dir}\\viewer-blob.bin"), vec![0u8; 5000]);
        let mut bmp: Vec<u8> = Vec::new();
        let (w, h) = (64u32, 48u32);
        bmp.extend_from_slice(b"BM");
        bmp.extend_from_slice(&(54 + w * h * 3).to_le_bytes());
        bmp.extend_from_slice(&[0, 0, 0, 0, 54, 0, 0, 0, 40, 0, 0, 0]);
        bmp.extend_from_slice(&w.to_le_bytes());
        bmp.extend_from_slice(&h.to_le_bytes());
        bmp.extend_from_slice(&[1, 0, 24, 0, 0, 0, 0, 0]);
        bmp.extend_from_slice(&(w * h * 3).to_le_bytes());
        bmp.extend_from_slice(&[0; 16]);
        bmp.extend((0..w * h).flat_map(|i| [(i % 255) as u8, 90, 200]));
        let _ = std::fs::write(format!("{dir}\\viewer-pic.bmp"), &bmp);
        if a().browser.cd(&dir, None, true) {
            a().browser.app_activated();
            let open = |name: &str| -> bool {
                let idx = a().browser.current.borrow().index_of(Some(name));
                if let Some(i) = idx {
                    a().browser.move_cursor(i as isize);
                    chars(hwnd, " ");
                    true
                } else {
                    false
                }
            };
            let wait_state = |want: &'static str| pump_until(Duration::from_secs(20), || a().viewer.state_name() == want);

            r.check("Space does not mark files", a().browser.selection.is_empty(), || "selection changed".into());
            if open("viewer-code.rs") {
                r.check("Space opens the viewer on code", a().viewer.active, || "not active".into());
                r.check("code file shows as text", wait_state("text"), || a().viewer.state_name().into());
                paint_now(hwnd);
                chars(hwnd, "G");
                let (first, total) = a().viewer.text_position().unwrap_or((0, 0));
                r.check("G scrolls the viewer to the end", total == 400 && first > 300, || format!("{first}/{total}"));
                chars(hwnd, "g");
                chars(hwnd, "j");
                r.check("g jumps to the top, j scrolls", a().viewer.text_position().map(|p| p.0) == Some(3), || format!("{:?}", a().viewer.text_position()));
                key(hwnd, VK_NEXT);
                r.check("PageDown scrolls a page", a().viewer.text_position().map(|p| p.0).unwrap_or(0) > 10, || format!("{:?}", a().viewer.text_position()));
                paint_now(hwnd);
                chars(hwnd, "q");
                r.check("q closes the viewer", !a().viewer.active, || "still active".into());
            } else {
                r.check("viewer-code.rs listed", false, || "missing".into());
            }
            if open("viewer-blob.bin") {
                r.check("binary file shows a hex view", wait_state("hex"), || a().viewer.state_name().into());
                paint_now(hwnd);
                key(hwnd, VK_ESCAPE);
                r.check("Esc closes the viewer", !a().viewer.active, || "still active".into());
            }
            if open("viewer-pic.bmp") {
                r.check("image file shows as an image", wait_state("image"), || a().viewer.state_name().into());
                paint_now(hwnd);
                chars(hwnd, "+");
                chars(hwnd, "0");
                chars(hwnd, "f");
                chars(hwnd, "f");
                let before = a().viewer.path.clone();
                chars(hwnd, "n");
                r.check("n opens the next file", a().viewer.path != before, || a().viewer.path.clone());
                chars(hwnd, "p");
                r.check("p goes back", a().viewer.path == before, || a().viewer.path.clone());
                paint_now(hwnd);
                chars(hwnd, " ");
                r.check("Space closes the viewer again", !a().viewer.active, || "still active".into());
            }
            paint_now(hwnd);
            r.check("main window renders after the viewer", a().gfx.ready() && !a().viewer.active, || "broken".into());
        }
        for n in ["viewer-code.rs", "viewer-blob.bin", "viewer-pic.bmp"] {
            let _ = std::fs::remove_file(format!("{dir}\\{n}"));
        }
    }
    // 10e. PDF viewer: a generated two-page PDF renders and scrolls.
    if let Ok(dir) = std::env::var("SM_START_PATH") {
        let content = "BT /F1 72 Tf 72 600 Td (HELLO) Tj ET";
        let objs: Vec<String> = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".into(),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> >>".into(),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
        }
        let xref = pdf.len();
        pdf.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).bytes());
        for off in offsets {
            pdf.extend(format!("{off:010} 00000 n \n").bytes());
        }
        pdf.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).bytes());
        let path = format!("{dir}\\viewer-doc.pdf");
        let _ = std::fs::write(&path, &pdf);
        if a().browser.cd(&dir, None, true) {
            a().browser.app_activated();
            let idx = a().browser.current.borrow().index_of(Some("viewer-doc.pdf"));
            if let Some(i) = idx {
                a().browser.move_cursor(i as isize);
                chars(hwnd, " ");
                let opened = pump_until(Duration::from_secs(20), || a().viewer.state_name() == "pdf");
                r.check("PDF opens in the viewer", opened, || a().viewer.state_name().into());
                // paint repeatedly: page renders are requested from paint and arrive as messages
                let rendered = pump_until(Duration::from_secs(20), || {
                    paint_now(hwnd);
                    a().viewer.pdf_info().map(|p| p.1 >= 1).unwrap_or(false)
                });
                r.check("PDF page is rendered", rendered, || format!("{:?}", a().viewer.pdf_info()));
                r.check("PDF reports its page count", a().viewer.pdf_info().map(|p| p.0) == Some(2), || format!("{:?}", a().viewer.pdf_info()));
                chars(hwnd, "G");
                paint_now(hwnd);
                r.check("G scrolls a PDF to the last page", a().viewer.pdf_info().map(|p| p.2) == Some(1), || format!("{:?}", a().viewer.pdf_info()));
                chars(hwnd, "g");
                paint_now(hwnd);
                r.check("g returns to the first page", a().viewer.pdf_info().map(|p| p.2) == Some(0), || format!("{:?}", a().viewer.pdf_info()));
                chars(hwnd, "+");
                chars(hwnd, "-");
                chars(hwnd, "0");
                paint_now(hwnd);
                chars(hwnd, "q");
                r.check("PDF viewer closes", !a().viewer.active, || "still active".into());
            } else {
                r.check("PDF file listed", false, || "missing".into());
            }
        }
        let _ = std::fs::remove_file(&path);
    }
    // 11. Final paint after everything, and the timer stops when idle.
    a().timer();
    paint_now(hwnd);
    r.check("still rendering at the end", a().gfx.ready(), || "lost".into());
}
