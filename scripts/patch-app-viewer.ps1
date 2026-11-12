# One-off patch script used while integrating the viewer into app.rs (kept for reference).
$f = "src/app.rs"
$t = [IO.File]::ReadAllText($f)
function Rep($old, $new) {
    if (-not $script:t.Contains($old)) { throw "missing: $old" }
    $script:t = $script:t.Replace($old, $new)
}

Rep @'
    elevated: bool,
'@ @'
    elevated: bool,
    viewer: viewer::Viewer,
    fs_saved: Option<(isize, WINDOWPLACEMENT)>,
'@

Rep @'
            elevated: shell::is_elevated(),
'@ @'
            elevated: shell::is_elevated(),
            viewer: viewer::Viewer::new(),
            fs_saved: None,
'@

Rep 'use crate::thumbs;' @'
use crate::thumbs;
use crate::viewer::{self, Act};
'@

Rep @'
        let lay = self.layout();
        self.hits.clear();
'@ @'
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
'@

Rep 'fn key_down(&mut self, vk: u16) -> bool {' 'fn key_down(&mut self, vk: u16, repeat: bool) -> bool {'

Rep @'
        if ctrl && !alt {
            if let Some(d) = zoom_action(vk) {
                self.change_zoom(d);
                return true;
            }
        }
        let key = match VIRTUAL_KEY(vk) {
'@ @'
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
'@

Rep 'fn char_input(&mut self, unit: u16) {' 'fn char_input(&mut self, unit: u16, repeat: bool) {'

Rep @'
        self.browser.handle_key(KeyEvent::ch(c));
        self.after_input();
    }

    fn timer(&mut self) {
'@ @'
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
'@

Rep @'
                Event::OpenRecycleBin => shell::open_recycle_bin(hwnd),
'@ @'
                Event::OpenRecycleBin => shell::open_recycle_bin(hwnd),
                Event::OpenViewer(p) => {
                    if let Some(a) = app() {
                        a.open_viewer(&p);
                    }
                }
'@

Rep @'
        WM_MOUSEMOVE => {
            let (x, y) = client_xy(lparam, scale);
            a.mouse_move(x, y);
            LRESULT(0)
        }
'@ @'
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
'@

Rep @'
            let (x, y) = client_xy(lparam, scale);
            a.click(x, y, msg == WM_LBUTTONDBLCLK);
'@ @'
            let (x, y) = client_xy(lparam, scale);
            if a.viewer.active {
                if a.viewer.mouse_down(&a.gfx, x, y) {
                    windows::Win32::UI::Input::KeyboardAndMouse::SetCapture(hwnd);
                }
                a.invalidate();
                return LRESULT(0);
            }
            a.click(x, y, msg == WM_LBUTTONDBLCLK);
'@

Rep @'
            let (x, y) = client_xy(lparam, scale);
            a.context_menu(x, y);
'@ @'
            let (x, y) = client_xy(lparam, scale);
            if a.viewer.active {
                return LRESULT(0);
            }
            a.context_menu(x, y);
'@

Rep @'
            if key_down(VK_CONTROL.0 as i32) {
                a.change_zoom(if delta > 0 { 1 } else { -1 });
            } else {
'@ @'
            if a.viewer.active {
                a.viewer.wheel(delta, key_down(VK_CONTROL.0 as i32));
                a.invalidate();
            } else if key_down(VK_CONTROL.0 as i32) {
                a.change_zoom(if delta > 0 { 1 } else { -1 });
            } else {
'@

Rep 'let handled = a.key_down(wparam.0 as u16);' 'let handled = a.key_down(wparam.0 as u16, (lparam.0 >> 30) & 1 == 1);'
Rep 'a.char_input(wparam.0 as u16);' 'a.char_input(wparam.0 as u16, (lparam.0 >> 30) & 1 == 1);'

Rep 'if (wparam.0 & 0xFFFF) != 0 && a.browser.deleting == 0 {' 'if (wparam.0 & 0xFFFF) != 0 && a.browser.deleting == 0 && !a.viewer.active {'

Rep @'
        thumbs::WM_THUMB => {
'@ @'
        viewer::WM_VIEWER => {
            let l = Box::from_raw(lparam.0 as *mut viewer::Loaded);
            let (dark, bg) = (a.theme.dark, a.theme.window);
            a.viewer.loaded(hwnd.0 as isize, *l, dark, bg);
            a.invalidate();
            a.ensure_timer();
            LRESULT(0)
        }
        thumbs::WM_THUMB => {
'@

[IO.File]::WriteAllText($f, $t)
"patched"
