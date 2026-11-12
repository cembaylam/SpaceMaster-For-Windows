//! Real WebView2 round trip: creation, rendering, script switch, scrolling, zoom, focus hand-back and timings.
//! Skipped (with a message) when the WebView2 Evergreen runtime is not installed.

use std::path::Path;
use std::time::{Duration, Instant};

use spacemaster::web::WebView;
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, PeekMessageW, RegisterClassW,
    TranslateMessage, MSG, PM_REMOVE, WNDCLASSW, WS_CLIPCHILDREN, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};

unsafe extern "system" fn wndproc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    DefWindowProcW(h, m, w, l)
}

fn make_window() -> HWND {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap();
        let class = w!("SpaceMasterWebTest");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        // Never shown: a top-level popup that stays hidden for the whole test.
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class,
            w!("web test"),
            WS_POPUP | WS_CLIPCHILDREN,
            0,
            0,
            800,
            600,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .expect("create test window")
    }
}

fn pump_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if done() {
            return true;
        }
        if start.elapsed() > timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn load(wv: &WebView, html: &str, scripts: bool) -> Duration {
    let t = Instant::now();
    wv.show_html(html, scripts);
    assert!(pump_until(Duration::from_secs(20), || wv.navigation_done()), "navigation never completed");
    t.elapsed()
}

fn num(wv: &WebView, js: &str) -> f64 {
    wv.eval(js, Duration::from_secs(5)).and_then(|s| s.trim().parse().ok()).unwrap_or_else(|| panic!("eval {js}"))
}

const DOC: &str = "<!doctype html><html><head><title>static</title></head>\
<body style=\"margin:0\"><h1 id=\"h\">Hello page</h1><div style=\"height:5000px\">tall</div>\
<script>document.title='js-ran'</script></body></html>";

#[test]
fn webview_end_to_end() {
    if !WebView::runtime_available() {
        println!("WebView2 runtime not installed - skipping web tests");
        return;
    }
    let parent = make_window();

    let t0 = Instant::now();
    let wv = WebView::new(parent.0 as isize).expect("create webview");
    assert!(!wv.is_ready(), "creation must be asynchronous");
    wv.set_background(0x1E1E1E);
    wv.set_rect(0, 0, 800, 600); // before ready: applied when ready
    wv.show(true);
    assert!(
        pump_until(Duration::from_secs(60), || wv.is_ready() || wv.error().is_some()),
        "webview was not ready in time"
    );
    assert!(wv.error().is_none(), "creation error: {:?}", wv.error());
    println!("creation (environment + controller): {} ms", t0.elapsed().as_millis());
    assert!(wv.is_ready());

    // scripts enabled: the page's script runs, content is rendered
    let first = load(&wv, DOC, true);
    println!("show_html -> NavigationCompleted (first, scripts on): {} ms", first.as_millis());
    assert_eq!(wv.eval_title().as_deref(), Some("js-ran"));
    assert_eq!(wv.eval("document.getElementById('h').textContent", Duration::from_secs(5)).as_deref(), Some("\"Hello page\""));

    // scripts disabled: the page's own script is blocked, but ExecuteScript still works
    let second = load(&wv, DOC, false);
    println!("show_html -> NavigationCompleted (second, scripts off): {} ms", second.as_millis());
    assert_eq!(wv.eval_title().as_deref(), Some("static"), "page script must not run in preview mode");
    assert_eq!(wv.eval("document.getElementById('h').textContent", Duration::from_secs(5)).as_deref(), Some("\"Hello page\""));

    // scrolling through scripts
    let third = load(&wv, DOC, true);
    println!("show_html -> NavigationCompleted (third, scripts on): {} ms", third.as_millis());
    let inner_h = num(&wv, "innerHeight");
    assert!(inner_h > 100.0, "innerHeight {inner_h}");
    assert_eq!(num(&wv, "scrollY"), 0.0);
    wv.scroll_by(0.0, 100.0);
    assert_eq!(num(&wv, "Math.round(scrollY)"), 100.0);
    wv.page_down();
    let after_page = num(&wv, "Math.round(scrollY)");
    assert!(
        (after_page - (100.0 + (inner_h * 0.9).round())).abs() <= 2.0,
        "page_down moved to {after_page} (innerHeight {inner_h})"
    );
    wv.page_up();
    assert!((num(&wv, "Math.round(scrollY)") - 100.0).abs() <= 2.0, "page_up");
    wv.scroll_to_bottom();
    let bottom = num(&wv, "Math.round(scrollY+innerHeight)");
    let total = num(&wv, "document.documentElement.scrollHeight");
    assert!((bottom - total).abs() <= 2.0, "scroll_to_bottom: {bottom} of {total}");
    wv.scroll_to_top();
    assert_eq!(num(&wv, "scrollY"), 0.0);

    // zoom round trip
    assert!((wv.zoom() - 1.0).abs() < 1e-9);
    wv.set_zoom(1.5);
    assert!((wv.zoom() - 1.5).abs() < 1e-6, "zoom {}", wv.zoom());
    wv.set_zoom(1.0);
    assert!((wv.zoom() - 1.0).abs() < 1e-6);

    // file navigation with a relative resource
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("web test {}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("rel.js"), "document.title='relative-ok'").unwrap();
    std::fs::write(dir.join("page.html"), "<html><body>file<script src=\"rel.js\"></script></body></html>").unwrap();
    let t = Instant::now();
    wv.navigate_file(&dir.join("page.html"), true);
    assert!(pump_until(Duration::from_secs(20), || wv.navigation_done()));
    println!("navigate_file -> NavigationCompleted: {} ms", t.elapsed().as_millis());
    assert_eq!(wv.eval_title().as_deref(), Some("relative-ok"));
    let _ = std::fs::remove_dir_all(&dir);

    // keystrokes stay with the parent: when the browser takes focus it is handed straight back
    unsafe {
        let _ = SetFocus(Some(parent));
    }
    let before = wv.focus_returns();
    wv.debug_take_focus();
    let returned = pump_until(Duration::from_secs(5), || wv.focus_returns() > before);
    pump_until(Duration::from_millis(200), || false);
    assert!(returned, "GotFocus was never reported / focus was not handed back");
    let focused = unsafe { GetFocus() };
    println!("focus returns: {}, focused window is parent: {}", wv.focus_returns() - before, focused == parent);
    assert_eq!(focused, parent, "focus must end up on the parent window");

    // hide / show / close
    wv.show(false);
    wv.show(true);
    wv.close();
    assert!(!wv.is_ready());
    drop(wv);
    unsafe {
        let _ = DestroyWindow(parent);
    }
}

#[test]
fn queued_calls_before_ready_do_not_panic() {
    if !WebView::runtime_available() {
        println!("WebView2 runtime not installed - skipping web tests");
        return;
    }
    let parent = make_window();
    let wv = WebView::new(parent.0 as isize).expect("create webview");
    wv.set_zoom(1.25);
    assert!((wv.zoom() - 1.25).abs() < 1e-9);
    wv.show_html("<title>queued</title><p>x</p>", false);
    wv.scroll_to_top();
    assert!(pump_until(Duration::from_secs(60), || wv.is_ready() || wv.error().is_some()));
    assert!(pump_until(Duration::from_secs(20), || wv.navigation_done()), "queued navigation never ran");
    assert_eq!(wv.eval_title().as_deref(), Some("queued"));
    assert!((wv.zoom() - 1.25).abs() < 1e-6);
    drop(wv);
    unsafe {
        let _ = DestroyWindow(parent);
    }
}
