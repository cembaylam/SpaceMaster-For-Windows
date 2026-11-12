//! Embedded HTML / Markdown renderer on top of WebView2 (Evergreen runtime).
//!
//! One environment + controller serves the whole session. They are created lazily by [`WebView::new`] (the
//! creation is asynchronous and finishes through the caller's ordinary Win32 message loop), after which the
//! page is only navigated, moved, hidden and shown. The browser never keeps keyboard focus: whenever it
//! receives focus it is handed straight back to the parent window, so every key stays with the host.
//!
//! All WebView2 types stay inside this module; the public surface is plain Rust types and `isize` window
//! handles. Everything must be used from the UI thread that created the [`WebView`].

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::{
    AcceleratorKeyPressedEventHandler, CreateCoreWebView2ControllerCompletedHandler,
    CreateCoreWebView2EnvironmentCompletedHandler, ExecuteScriptCompletedHandler, FocusChangedEventHandler,
    MoveFocusRequestedEventHandler, NavigationCompletedEventHandler, NavigationStartingEventHandler,
    NewWindowRequestedEventHandler,
};
use windows::core::{Interface, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
};

/// Scrolling step of `j` / `k` style keys, in CSS pixels (same as FileMaster).
const LINE_STEP: f64 = 96.0;

fn hwnd_of(h: isize) -> HWND {
    HWND(h as *mut core::ffi::c_void)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Navigation requested before the controller exists.
enum PendingNav {
    Html(String, bool),
    Url(String, bool),
}

struct Inner {
    parent: isize,
    env: RefCell<Option<ICoreWebView2Environment>>,
    controller: RefCell<Option<ICoreWebView2Controller>>,
    view: RefCell<Option<ICoreWebView2>>,
    ready: Cell<bool>,
    failed: RefCell<Option<String>>,
    closed: Cell<bool>,
    rect: Cell<(i32, i32, i32, i32)>,
    visible: Cell<bool>,
    zoom: Cell<f64>,
    background: Cell<u32>,
    pending_nav: RefCell<Option<PendingNav>>,
    pending_scripts: RefCell<Vec<String>>,
    nav_done: Cell<bool>,
    nav_inflight: Cell<u32>,
    focus_returns: Cell<u32>,
}

impl Inner {
    fn controller(&self) -> Option<ICoreWebView2Controller> {
        self.controller.borrow().clone()
    }

    fn view(&self) -> Option<ICoreWebView2> {
        self.view.borrow().clone()
    }

    fn apply_bounds(&self) {
        if let Some(c) = self.controller() {
            let (x, y, w, h) = self.rect.get();
            let r = RECT { left: x, top: y, right: x + w.max(0), bottom: y + h.max(0) };
            unsafe {
                let _ = c.SetBounds(r);
            }
        }
    }

    fn apply_visible(&self) {
        if let Some(c) = self.controller() {
            unsafe {
                let _ = c.SetIsVisible(self.visible.get());
            }
        }
    }

    fn apply_background(&self) {
        if let Some(c) = self.controller() {
            let rgb = self.background.get();
            if let Ok(c2) = c.cast::<ICoreWebView2Controller2>() {
                let color = COREWEBVIEW2_COLOR {
                    A: 255,
                    R: ((rgb >> 16) & 0xFF) as u8,
                    G: ((rgb >> 8) & 0xFF) as u8,
                    B: (rgb & 0xFF) as u8,
                };
                unsafe {
                    let _ = c2.SetDefaultBackgroundColor(color);
                }
            }
        }
    }

    fn set_scripts(&self, view: &ICoreWebView2, enabled: bool) {
        unsafe {
            if let Ok(s) = view.Settings() {
                let _ = s.SetIsScriptEnabled(enabled);
            }
        }
    }

    fn navigate(&self, nav: PendingNav) {
        let Some(view) = self.view() else {
            *self.pending_nav.borrow_mut() = Some(nav);
            return;
        };
        let ok = unsafe {
            match &nav {
                PendingNav::Html(html, scripts) => {
                    self.set_scripts(&view, *scripts);
                    view.NavigateToString(PCWSTR(wide(html).as_ptr()))
                }
                PendingNav::Url(url, scripts) => {
                    self.set_scripts(&view, *scripts);
                    view.Navigate(PCWSTR(wide(url).as_ptr()))
                }
            }
        };
        if ok.is_err() {
            self.nav_done.set(true);
        }
    }

    fn run_script(&self, js: &str) {
        match self.view() {
            Some(view) => unsafe {
                let handler = ExecuteScriptCompletedHandler::create(Box::new(|_, _| Ok(())));
                let _ = view.ExecuteScript(PCWSTR(wide(js).as_ptr()), &handler);
            },
            None => {
                if !self.closed.get() {
                    self.pending_scripts.borrow_mut().push(js.to_owned());
                }
            }
        }
    }

    fn give_focus_back(&self) {
        self.focus_returns.set(self.focus_returns.get() + 1);
        unsafe {
            let _ = SetFocus(Some(hwnd_of(self.parent)));
        }
    }

    /// Controller exists: configure it, hook the events, flush whatever was queued.
    fn on_controller(self: &Rc<Self>, controller: ICoreWebView2Controller) {
        if self.closed.get() {
            unsafe {
                let _ = controller.Close();
            }
            return;
        }
        let view = match unsafe { controller.CoreWebView2() } {
            Ok(v) => v,
            Err(e) => {
                *self.failed.borrow_mut() = Some(format!("CoreWebView2: {e}"));
                return;
            }
        };

        unsafe {
            if let Ok(s) = view.Settings() {
                let _ = s.SetAreDefaultContextMenusEnabled(false);
                let _ = s.SetAreDevToolsEnabled(false);
                let _ = s.SetIsStatusBarEnabled(false);
                let _ = s.SetIsZoomControlEnabled(false);
                if let Ok(s3) = s.cast::<ICoreWebView2Settings3>() {
                    let _ = s3.SetAreBrowserAcceleratorKeysEnabled(false);
                }
                if let Ok(s4) = s.cast::<ICoreWebView2Settings4>() {
                    let _ = s4.SetIsGeneralAutofillEnabled(false);
                    let _ = s4.SetIsPasswordAutosaveEnabled(false);
                }
                if let Ok(s5) = s.cast::<ICoreWebView2Settings5>() {
                    let _ = s5.SetIsPinchZoomEnabled(false);
                }
            }

            let mut token = 0i64;

            // Focus always goes back to the host window.
            let weak: Weak<Inner> = Rc::downgrade(self);
            let h = FocusChangedEventHandler::create(Box::new(move |_, _| {
                if let Some(me) = weak.upgrade() {
                    me.give_focus_back();
                }
                Ok(())
            }));
            let _ = controller.add_GotFocus(&h, &mut token);

            // Tab / Shift+Tab leaving the page: swallow it and keep focus on the host.
            let weak: Weak<Inner> = Rc::downgrade(self);
            let h = MoveFocusRequestedEventHandler::create(Box::new(move |_, args| {
                if let Some(args) = args {
                    let _ = args.SetHandled(true);
                }
                if let Some(me) = weak.upgrade() {
                    me.give_focus_back();
                }
                Ok(())
            }));
            let _ = controller.add_MoveFocusRequested(&h, &mut token);

            // Never let the page swallow accelerator keys.
            let h = AcceleratorKeyPressedEventHandler::create(Box::new(|_, args| {
                if let Some(args) = args {
                    let _ = args.SetHandled(false);
                }
                Ok(())
            }));
            let _ = controller.add_AcceleratorKeyPressed(&h, &mut token);

            // Navigation bookkeeping.
            let weak: Weak<Inner> = Rc::downgrade(self);
            let h = NavigationStartingEventHandler::create(Box::new(move |_, _| {
                if let Some(me) = weak.upgrade() {
                    me.nav_inflight.set(me.nav_inflight.get() + 1);
                    me.nav_done.set(false);
                }
                Ok(())
            }));
            let _ = view.add_NavigationStarting(&h, &mut token);

            let weak: Weak<Inner> = Rc::downgrade(self);
            let h = NavigationCompletedEventHandler::create(Box::new(move |_, _| {
                if let Some(me) = weak.upgrade() {
                    let n = me.nav_inflight.get().saturating_sub(1);
                    me.nav_inflight.set(n);
                    if n == 0 {
                        me.nav_done.set(true);
                    }
                }
                Ok(())
            }));
            let _ = view.add_NavigationCompleted(&h, &mut token);

            // target=_blank and window.open must not spawn browser windows.
            let h = NewWindowRequestedEventHandler::create(Box::new(|_, args| {
                if let Some(args) = args {
                    let _ = args.SetHandled(true);
                }
                Ok(())
            }));
            let _ = view.add_NewWindowRequested(&h, &mut token);
        }

        *self.controller.borrow_mut() = Some(controller.clone());
        *self.view.borrow_mut() = Some(view);
        unsafe {
            let _ = controller.SetZoomFactor(self.zoom.get());
        }
        self.apply_background();
        self.apply_bounds();
        self.apply_visible();
        self.ready.set(true);

        if let Some(nav) = self.pending_nav.borrow_mut().take() {
            self.navigate(nav);
        }
        let scripts: Vec<String> = std::mem::take(&mut *self.pending_scripts.borrow_mut());
        for js in scripts {
            self.run_script(&js);
        }
    }
}

/// The session-wide embedded browser. See the module documentation.
pub struct WebView {
    inner: Rc<Inner>,
}

impl WebView {
    /// Is the WebView2 Evergreen runtime installed?
    pub fn runtime_available() -> bool {
        unsafe {
            let mut version = PWSTR::null();
            let ok = GetAvailableCoreWebView2BrowserVersionString(PCWSTR::null(), &mut version).is_ok();
            let present = ok && !version.is_null() && *version.0 != 0;
            if !version.is_null() {
                CoTaskMemFree(Some(version.0 as *const _));
            }
            present
        }
    }

    /// Starts the asynchronous creation of the environment and controller as a hidden child of `parent_hwnd`.
    /// Completion is delivered through the caller's normal message loop; poll [`WebView::is_ready`].
    pub fn new(parent_hwnd: isize) -> Result<WebView, String> {
        if parent_hwnd == 0 {
            return Err("no parent window".into());
        }
        let data_dir = std::env::var_os("LOCALAPPDATA")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("SpaceMaster")
            .join("webview2");
        let _ = std::fs::create_dir_all(&data_dir);

        let inner = Rc::new(Inner {
            parent: parent_hwnd,
            env: RefCell::new(None),
            controller: RefCell::new(None),
            view: RefCell::new(None),
            ready: Cell::new(false),
            failed: RefCell::new(None),
            closed: Cell::new(false),
            rect: Cell::new((0, 0, 0, 0)),
            visible: Cell::new(false),
            zoom: Cell::new(1.0),
            background: Cell::new(0x000000),
            pending_nav: RefCell::new(None),
            pending_scripts: RefCell::new(Vec::new()),
            nav_done: Cell::new(false),
            nav_inflight: Cell::new(0),
            focus_returns: Cell::new(0),
        });

        unsafe {
            // WebView2 needs a single-threaded COM apartment; a different mode already set is fine.
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }

        let weak: Weak<Inner> = Rc::downgrade(&inner);
        let parent = hwnd_of(parent_hwnd);
        let handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(move |hr, env| {
            let Some(me) = weak.upgrade() else { return Ok(()) };
            if let Err(e) = hr {
                *me.failed.borrow_mut() = Some(format!("environment: {e}"));
                return Ok(());
            }
            let Some(env) = env else {
                *me.failed.borrow_mut() = Some("environment: null".into());
                return Ok(());
            };
            *me.env.borrow_mut() = Some(env.clone());
            let weak2 = Rc::downgrade(&me);
            let ch = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(move |hr, controller| {
                let Some(me) = weak2.upgrade() else {
                    if let Some(c) = controller {
                        unsafe {
                            let _ = c.Close();
                        }
                    }
                    return Ok(());
                };
                match (hr, controller) {
                    (Ok(()), Some(c)) => me.on_controller(c),
                    (Err(e), _) => *me.failed.borrow_mut() = Some(format!("controller: {e}")),
                    _ => *me.failed.borrow_mut() = Some("controller: null".into()),
                }
                Ok(())
            }));
            if let Err(e) = unsafe { env.CreateCoreWebView2Controller(parent, &ch) } {
                *me.failed.borrow_mut() = Some(format!("controller: {e}"));
            }
            Ok(())
        }));

        let folder = wide(&data_dir.to_string_lossy());
        let r = unsafe {
            CreateCoreWebView2EnvironmentWithOptions(
                PCWSTR::null(),
                PCWSTR(folder.as_ptr()),
                None::<&ICoreWebView2EnvironmentOptions>,
                &handler,
            )
        };
        r.map_err(|e| format!("WebView2 environment: {e}"))?;
        Ok(WebView { inner })
    }

    /// Creation finished and the controller is usable.
    pub fn is_ready(&self) -> bool {
        self.inner.ready.get()
    }

    /// Why creation failed, if it did (callbacks arrive after `new` returned).
    pub fn error(&self) -> Option<String> {
        self.inner.failed.borrow().clone()
    }

    /// Position inside the parent's client area, in pixels. May be called before the view is ready.
    pub fn set_rect(&self, x: i32, y: i32, w: i32, h: i32) {
        self.inner.rect.set((x, y, w, h));
        self.inner.apply_bounds();
    }

    pub fn show(&self, visible: bool) {
        self.inner.visible.set(visible);
        self.inner.apply_visible();
    }

    /// Renders a ready HTML string. `scripts_enabled = false` for the small preview pane.
    pub fn show_html(&self, html: &str, scripts_enabled: bool) {
        self.inner.nav_done.set(false);
        self.inner.navigate(PendingNav::Html(html.to_owned(), scripts_enabled));
    }

    /// Navigates to a local file (`file://`), so relative resources of an `.html` file resolve.
    pub fn navigate_file(&self, path: &Path, scripts_enabled: bool) {
        self.inner.nav_done.set(false);
        self.inner.navigate(PendingNav::Url(file_url(path), scripts_enabled));
    }

    /// Fire-and-forget script execution. Scripts run even when the page's own scripts are disabled.
    pub fn run_script(&self, js: &str) {
        self.inner.run_script(js);
    }

    pub fn set_zoom(&self, factor: f64) {
        let f = if factor.is_finite() { factor.clamp(0.25, 5.0) } else { 1.0 };
        self.inner.zoom.set(f);
        if let Some(c) = self.inner.controller() {
            unsafe {
                let _ = c.SetZoomFactor(f);
            }
        }
    }

    pub fn zoom(&self) -> f64 {
        if let Some(c) = self.inner.controller() {
            let mut f = 0.0f64;
            if unsafe { c.ZoomFactor(&mut f) }.is_ok() && f > 0.0 {
                return f;
            }
        }
        self.inner.zoom.get()
    }

    /// `0xRRGGBB`, shown before the page paints so dark mode does not flash white.
    pub fn set_background(&self, rgb: u32) {
        self.inner.background.set(rgb & 0x00FF_FFFF);
        self.inner.apply_background();
    }

    pub fn scroll_by(&self, dx: f64, dy: f64) {
        self.run_script(&format!("scrollBy({},{})", num(dx), num(dy)));
    }

    pub fn scroll_line_up(&self) {
        self.scroll_by(0.0, -LINE_STEP);
    }

    pub fn scroll_line_down(&self) {
        self.scroll_by(0.0, LINE_STEP);
    }

    pub fn scroll_to_top(&self) {
        self.run_script("scrollTo(0,0)");
    }

    pub fn scroll_to_bottom(&self) {
        self.run_script("scrollTo(0,document.documentElement.scrollHeight)");
    }

    pub fn page_up(&self) {
        self.run_script("scrollBy(0,-innerHeight*0.9)");
    }

    pub fn page_down(&self) {
        self.run_script("scrollBy(0,innerHeight*0.9)");
    }

    /// The last navigation has completed (NavigationCompleted arrived).
    pub fn navigation_done(&self) -> bool {
        self.inner.nav_done.get()
    }

    /// Releases the controller. Also done on drop.
    pub fn close(&self) {
        let i = &self.inner;
        i.closed.set(true);
        i.ready.set(false);
        i.pending_nav.borrow_mut().take();
        i.pending_scripts.borrow_mut().clear();
        let c = i.controller.borrow_mut().take();
        i.view.borrow_mut().take();
        i.env.borrow_mut().take();
        if let Some(c) = c {
            unsafe {
                let _ = c.Close();
            }
        }
    }

    // ---- test helpers -------------------------------------------------------------------------------------

    /// Runs a script and returns its JSON-encoded result, pumping messages meanwhile.
    #[doc(hidden)]
    pub fn eval(&self, js: &str, timeout: Duration) -> Option<String> {
        let view = self.inner.view()?;
        let slot: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let s2 = slot.clone();
        let handler = ExecuteScriptCompletedHandler::create(Box::new(move |hr, result| {
            *s2.borrow_mut() = Some(if hr.is_ok() { result } else { String::new() });
            Ok(())
        }));
        unsafe { view.ExecuteScript(PCWSTR(wide(js).as_ptr()), &handler) }.ok()?;
        let start = Instant::now();
        while slot.borrow().is_none() && start.elapsed() < timeout {
            pump_messages();
            std::thread::sleep(Duration::from_millis(1));
        }
        let r = slot.borrow_mut().take();
        r
    }

    /// `document.title` of the current page.
    #[doc(hidden)]
    pub fn eval_title(&self) -> Option<String> {
        let json = self.eval("document.title", Duration::from_secs(5))?;
        Some(json_unquote(&json))
    }

    /// Asks the browser to take keyboard focus (as a click would).
    #[doc(hidden)]
    pub fn debug_take_focus(&self) {
        if let Some(c) = self.inner.controller() {
            unsafe {
                let _ = c.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC);
            }
        }
    }

    /// How many times focus was handed back to the parent.
    #[doc(hidden)]
    pub fn focus_returns(&self) -> u32 {
        self.inner.focus_returns.get()
    }
}

impl Drop for WebView {
    fn drop(&mut self) {
        self.close();
    }
}

fn pump_messages() {
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn num(v: f64) -> String {
    if v.is_finite() {
        format!("{v}")
    } else {
        "0".into()
    }
}

/// `file:///C:/dir/a%20b.html`, `file://server/share/x.html` for UNC paths.
fn file_url(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let s = s.strip_prefix("//?/").unwrap_or(&s);
    let (prefix, rest) = if let Some(unc) = s.strip_prefix("UNC/") {
        ("file://", unc.to_owned())
    } else if let Some(unc) = s.strip_prefix("//") {
        ("file://", unc.to_owned())
    } else {
        ("file:///", s.to_owned())
    };
    let mut out = String::from(prefix);
    for b in rest.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' | b':' | b'!' | b'$' | b'&'
            | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'=' | b'@' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn json_unquote(s: &str) -> String {
    let t = s.trim();
    let t = t.strip_prefix('"').and_then(|x| x.strip_suffix('"')).unwrap_or(t);
    let mut out = String::new();
    let mut it = t.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('u') => {
                let hex: String = it.by_ref().take(4).collect();
                if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    out.push(ch);
                }
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls_are_encoded() {
        assert_eq!(file_url(Path::new(r"C:\a b\ç.html")), "file:///C:/a%20b/%C3%A7.html");
        assert_eq!(file_url(Path::new(r"\\?\C:\x.html")), "file:///C:/x.html");
        assert_eq!(file_url(Path::new(r"\\srv\sh\x.html")), "file://srv/sh/x.html");
    }

    #[test]
    fn json_strings_unquote() {
        assert_eq!(json_unquote(r#""a\"b\\c\u00e7""#), "a\"b\\c\u{e7}");
    }
}
