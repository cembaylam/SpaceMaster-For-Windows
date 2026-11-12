//! LibVLC player test: needs the engine and a real video (env SM_TEST_VIDEO, else the smallest clip in Downloads).
//! Skips with a message when either is missing.

use std::ffi::c_void;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use spacemaster::video::Player;
use windows::core::w;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    WINDOW_EX_STYLE, WS_POPUP,
};

fn pump() {
    unsafe {
        let mut m = MSG::default();
        while PeekMessageW(&mut m, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&m);
            DispatchMessageW(&m);
        }
    }
}

fn wait_until(timeout_ms: u64, mut cond: impl FnMut() -> bool) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_millis(timeout_ms) {
        pump();
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    pump();
    cond()
}

fn find_video() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SM_TEST_VIDEO") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let dir = std::path::Path::new(r"C:\Users\Cem\Downloads");
    let mut best: Option<(u64, PathBuf)> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("").to_ascii_lowercase();
        if !["mp4", "mkv", "mov", "avi"].contains(&ext.as_str()) {
            continue;
        }
        if let Ok(md) = e.metadata() {
            if md.is_file() && best.as_ref().map_or(true, |(s, _)| md.len() < *s) {
                best = Some((md.len(), p));
            }
        }
    }
    best.map(|(_, p)| p)
}

#[test]
fn player_end_to_end() {
    if !Player::engine_available() {
        println!("SKIP: libvlc engine not found (set SM_LIBVLC_DIR)");
        return;
    }
    let Some(video) = find_video() else {
        println!("SKIP: no test video (set SM_TEST_VIDEO)");
        return;
    };
    println!("video: {}", video.display());

    let parent = unsafe {
        CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), w!("vt"), WS_POPUP, 0, 0, 640, 360, None, None, None, None)
    }
    .expect("parent window");

    {
        let t0 = Instant::now();
        let mut p = Player::new(parent.0 as isize).expect("Player::new");
        println!("Player::new: {} ms", t0.elapsed().as_millis());
        p.set_rect(0, 0, 640, 360);

        let t0 = Instant::now();
        p.load(&video).expect("load");
        assert!(!p.frame_ready() || t0.elapsed().as_millis() > 0);
        assert!(wait_until(8000, || p.frame_ready()), "frame_ready within 8 s");
        println!("load -> frame_ready: {} ms", t0.elapsed().as_millis());
        p.show(true);

        assert!(wait_until(3000, || p.length_ms() > 0), "length known");
        let len = p.length_ms();
        println!("length {} ms, has_video {}, size {:?}", len, p.has_video(), p.video_size());
        assert!(p.is_playing());

        // time advances
        let t1 = p.time_ms();
        assert!(wait_until(3000, || p.time_ms() > t1 + 150), "time advances");

        // pause
        p.toggle_pause();
        assert!(wait_until(2000, || !p.is_playing()), "paused");
        std::thread::sleep(Duration::from_millis(200));
        pump();
        let tp = p.time_ms();
        let t_wait = Instant::now();
        while t_wait.elapsed() < Duration::from_millis(400) {
            pump();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!((p.time_ms() - tp).abs() < 100, "clock frozen while paused");
        p.toggle_pause();
        assert!(wait_until(2000, || p.is_playing()), "resumed");

        // seek lands near the target (keyframe precision)
        let target = len / 2;
        p.seek_to_ms(target);
        assert!(p.recently_sought());
        assert_eq!(p.last_seek_target(), Some(target.clamp(0, len - 300)));
        let tol = (len / 5).max(2500);
        assert!(wait_until(3000, || (p.time_ms() - target).abs() < tol), "seek near target");
        println!("seek target {} -> clock {}", target, p.time_ms());
        std::thread::sleep(Duration::from_millis(400));
        assert!(!p.recently_sought());
        // clamps
        p.seek_to_ms(len * 10);
        assert_eq!(p.last_seek_target(), Some(len - 300));
        p.seek_to_ms(-5);
        assert_eq!(p.last_seek_target(), Some(0));
        wait_until(1500, || false);

        // volume / mute
        p.set_volume(80);
        assert_eq!(p.volume(), 80);
        p.set_volume(999);
        assert_eq!(p.volume(), 150);
        p.set_volume(100);
        p.set_muted(true);
        assert!(p.muted());
        p.set_muted(false);
        assert!(!p.muted());

        // seek latency: alternate between 20% and 70%, wait until the clock is nearer the new target
        if len >= 4000 {
            let (a, b) = (len / 5, len * 7 / 10);
            let mut lat = Vec::new();
            for i in 0..20 {
                let (from, to) = if i % 2 == 0 { (b, a) } else { (a, b) };
                if i == 0 {
                    p.seek_to_ms(from);
                    wait_until(1500, || (p.time_ms() - from).abs() < (to - from).abs() / 2);
                    std::thread::sleep(Duration::from_millis(400));
                }
                let t0 = Instant::now();
                p.seek_to_ms(to);
                // libvlc reports the seek target right away; the real signal is the clock starting to run
                // again from the new position (value changes while staying near the target)
                let v0 = p.time_ms();
                let ok = wait_until(2000, || {
                    let t = p.time_ms();
                    t != v0 && (t - to).abs() < (to - from).abs() / 2
                });
                lat.push(t0.elapsed().as_secs_f64() * 1000.0);
                assert!(ok, "seek {i} landed");
                // let the pipeline settle like a human key press interval
                wait_until(150, || false);
            }
            lat.sort_by(|x, y| x.partial_cmp(y).unwrap());
            let avg: f64 = lat.iter().sum::<f64>() / lat.len() as f64;
            println!(
                "seek-to-playing-at-new-position latency over 20 seeks: min {:.1} ms, median {:.1} ms, avg {:.1} ms, max {:.1} ms",
                lat[0],
                lat[lat.len() / 2],
                avg,
                lat[lat.len() - 1]
            );
        } else {
            println!("video too short for latency run");
        }

        // load a second time (re-hides until the first frame again)
        p.load(&video).expect("reload");
        assert!(wait_until(8000, || p.frame_ready()), "frame_ready after reload");

        p.stop();
        assert!(!p.is_playing());
        assert!(!p.frame_ready());
        // drop while playing must be safe
        p.load(&video).expect("load before drop");
        wait_until(1500, || p.frame_ready());
    }
    unsafe {
        let _ = DestroyWindow(HWND(parent.0 as *mut c_void));
    }
}
