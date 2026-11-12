//! Built-in video/audio player on LibVLC, loaded at run time (no link-time dependency).
//!
//! Same design as the proven FileMaster player: `--input-fast-seek` (seeks land on the nearest keyframe instead of
//! decoding up to the target) and `--file-caching=100` (VLC refills this much read-ahead after every seek; the
//! 300 ms default is the main seek latency) give about 50-80 ms from key press to the new picture.
//!
//! Video is rendered by VLC into a child window of the parent HWND. VLC's own mouse and keyboard handling is
//! switched off so keystrokes always stay with the parent window.
//!
//! Clock semantics: right after a seek the player clock (`time_ms`) still reports the OLD time for a short while.
//! Callers that show a position (progress bar, scrub logic) should show `last_seek_target()` while
//! `recently_sought()` is true (350 ms window) and chain relative seeks from that target, not from the stale clock.

#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, c_int, c_uint, c_void, CString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use windows::core::{w, PCSTR, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND};
use windows::Win32::Graphics::Gdi::{GetStockObject, BLACK_BRUSH, HBRUSH};
use windows::Win32::System::LibraryLoader::{
    GetModuleHandleW, GetProcAddress, LoadLibraryExW, LOAD_WITH_ALTERED_SEARCH_PATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassExW, SetWindowPos, ShowWindow,
    SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE, SW_SHOWNA, WINDOW_EX_STYLE, WNDCLASSEXW, WS_CHILD,
    WS_CLIPCHILDREN, WS_CLIPSIBLINGS,
};

// ------------------------------------------------------------------ libvlc binding

type Instance = *mut c_void;
type MediaPlayer = *mut c_void;
type Media = *mut c_void;
type EventManager = *mut c_void;
type EventCb = unsafe extern "C" fn(*const c_void, *mut c_void);

// libvlc 3.x event types (libvlc_event_e)
const EV_ENCOUNTERED_ERROR: c_int = 266;
const EV_TIME_CHANGED: c_int = 267;
const EV_END_REACHED: c_int = 265;

// libvlc_state_t
const ST_NOTHING: c_int = 0;
const ST_STOPPED: c_int = 5;

struct Api {
    new: unsafe extern "C" fn(c_int, *const *const c_char) -> Instance,
    release: unsafe extern "C" fn(Instance),
    media_new_path: unsafe extern "C" fn(Instance, *const c_char) -> Media,
    media_release: unsafe extern "C" fn(Media),
    mp_new: unsafe extern "C" fn(Instance) -> MediaPlayer,
    mp_release: unsafe extern "C" fn(MediaPlayer),
    mp_set_media: unsafe extern "C" fn(MediaPlayer, Media),
    mp_play: unsafe extern "C" fn(MediaPlayer) -> c_int,
    mp_stop: unsafe extern "C" fn(MediaPlayer),
    mp_set_hwnd: unsafe extern "C" fn(MediaPlayer, *mut c_void),
    mp_set_pause: unsafe extern "C" fn(MediaPlayer, c_int),
    mp_is_playing: unsafe extern "C" fn(MediaPlayer) -> c_int,
    mp_get_state: unsafe extern "C" fn(MediaPlayer) -> c_int,
    mp_get_time: unsafe extern "C" fn(MediaPlayer) -> i64,
    mp_set_time: unsafe extern "C" fn(MediaPlayer, i64),
    mp_get_length: unsafe extern "C" fn(MediaPlayer) -> i64,
    mp_has_vout: unsafe extern "C" fn(MediaPlayer) -> c_uint,
    mp_event_manager: unsafe extern "C" fn(MediaPlayer) -> EventManager,
    ev_attach: unsafe extern "C" fn(EventManager, c_int, EventCb, *mut c_void) -> c_int,
    ev_detach: unsafe extern "C" fn(EventManager, c_int, EventCb, *mut c_void),
    audio_set_volume: unsafe extern "C" fn(MediaPlayer, c_int) -> c_int,
    audio_set_mute: unsafe extern "C" fn(MediaPlayer, c_int),
    video_get_size: unsafe extern "C" fn(MediaPlayer, c_uint, *mut c_uint, *mut c_uint) -> c_int,
    video_set_mouse_input: unsafe extern "C" fn(MediaPlayer, c_uint),
    video_set_key_input: unsafe extern "C" fn(MediaPlayer, c_uint),
}

fn engine_dir() -> Option<&'static PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let mut cands: Vec<PathBuf> = Vec::new();
        if let Some(d) = std::env::var_os("SM_LIBVLC_DIR") {
            if !d.is_empty() {
                cands.push(PathBuf::from(d));
            }
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                cands.push(dir.join("libvlc").join("win-x64"));
            }
        }
        cands
            .into_iter()
            .find(|d| d.join("libvlc.dll").is_file() && d.join("libvlccore.dll").is_file() && d.join("plugins").is_dir())
    })
    .as_ref()
}

fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(load_api).as_ref().map_err(|e| e.clone())
}

fn load_api() -> Result<Api, String> {
    use std::os::windows::ffi::OsStrExt;
    let dir = engine_dir().ok_or_else(|| "libvlc not found".to_string())?;
    // plugins are found through this env var as well as the explicit --plugin-path option
    std::env::set_var("VLC_PLUGIN_PATH", dir.join("plugins"));
    let dll = dir.join("libvlc.dll");
    let wide: Vec<u16> = dll.as_os_str().encode_wide().chain(Some(0)).collect();
    let h = unsafe { LoadLibraryExW(PCWSTR(wide.as_ptr()), None, LOAD_WITH_ALTERED_SEARCH_PATH) }
        .map_err(|e| format!("cannot load libvlc.dll: {e}"))?;

    macro_rules! sym {
        ($name:literal) => {{
            let p = unsafe { GetProcAddress(h, PCSTR(concat!($name, "\0").as_ptr())) };
            match p {
                Some(f) => unsafe { std::mem::transmute(f) },
                None => return Err(format!("libvlc symbol missing: {}", $name)),
            }
        }};
    }
    Ok(Api {
        new: sym!("libvlc_new"),
        release: sym!("libvlc_release"),
        media_new_path: sym!("libvlc_media_new_path"),
        media_release: sym!("libvlc_media_release"),
        mp_new: sym!("libvlc_media_player_new"),
        mp_release: sym!("libvlc_media_player_release"),
        mp_set_media: sym!("libvlc_media_player_set_media"),
        mp_play: sym!("libvlc_media_player_play"),
        mp_stop: sym!("libvlc_media_player_stop"),
        mp_set_hwnd: sym!("libvlc_media_player_set_hwnd"),
        mp_set_pause: sym!("libvlc_media_player_set_pause"),
        mp_is_playing: sym!("libvlc_media_player_is_playing"),
        mp_get_state: sym!("libvlc_media_player_get_state"),
        mp_get_time: sym!("libvlc_media_player_get_time"),
        mp_set_time: sym!("libvlc_media_player_set_time"),
        mp_get_length: sym!("libvlc_media_player_get_length"),
        mp_has_vout: sym!("libvlc_media_player_has_vout"),
        mp_event_manager: sym!("libvlc_media_player_event_manager"),
        ev_attach: sym!("libvlc_event_attach"),
        ev_detach: sym!("libvlc_event_detach"),
        audio_set_volume: sym!("libvlc_audio_set_volume"),
        audio_set_mute: sym!("libvlc_audio_set_mute"),
        video_get_size: sym!("libvlc_video_get_size"),
        video_set_mouse_input: sym!("libvlc_video_set_mouse_input"),
        video_set_key_input: sym!("libvlc_video_set_key_input"),
    })
}

// ------------------------------------------------------------------ child window

unsafe extern "system" fn video_wndproc(
    hwnd: HWND,
    msg: u32,
    wp: windows::Win32::Foundation::WPARAM,
    lp: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    DefWindowProcW(hwnd, msg, wp, lp)
}

fn register_class() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| unsafe {
        let hi = GetModuleHandleW(None).map(|m| HINSTANCE(m.0)).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(video_wndproc),
            hInstance: hi,
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            lpszClassName: w!("SpaceMasterVideo"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
    });
}

// ------------------------------------------------------------------ player

/// State shared with libvlc's event thread. Callbacks only touch atomics.
struct Shared {
    frame_ready: AtomicBool,
    ended: AtomicBool,
    errored: AtomicBool,
    last_seek_target: AtomicI64, // -1 = none
    last_seek_tick: AtomicI64,   // ms since process clock start, -1 = never
    volume: AtomicI32,
    muted: AtomicBool,
    audio_applied: AtomicBool,
}

fn now_ms() -> i64 {
    static T0: OnceLock<Instant> = OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_millis() as i64
}

unsafe extern "C" fn on_event(ev: *const c_void, data: *mut c_void) {
    if ev.is_null() || data.is_null() {
        return;
    }
    // libvlc_event_t begins with `int type`
    let ty = *(ev as *const c_int);
    let sh = &*(data as *const Shared);
    match ty {
        // like FileMaster's _videoPending: the first clock tick means the first frame (or audio) is out
        EV_TIME_CHANGED => sh.frame_ready.store(true, Ordering::Release),
        EV_END_REACHED => sh.ended.store(true, Ordering::Release),
        EV_ENCOUNTERED_ERROR => sh.errored.store(true, Ordering::Release),
        _ => {}
    }
}

const EVENTS: [c_int; 3] = [EV_TIME_CHANGED, EV_END_REACHED, EV_ENCOUNTERED_ERROR];

pub struct Player {
    api: &'static Api,
    hwnd: HWND,
    inst: Instance,
    mp: MediaPlayer,
    sh: Arc<Shared>,
    lock: Mutex<()>, // serialises load/stop against each other
}

impl Player {
    /// Is the libvlc engine present (libvlc.dll, libvlccore.dll and plugins)?
    pub fn engine_available() -> bool {
        engine_dir().is_some()
    }

    /// Creates the (hidden) child window, the libvlc instance and the media player.
    pub fn new(parent_hwnd: isize) -> Result<Player, String> {
        let api = api()?;
        register_class();

        let hwnd = unsafe {
            let hi = GetModuleHandleW(None).map(|m| HINSTANCE(m.0)).unwrap_or_default();
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("SpaceMasterVideo"),
                w!(""),
                WS_CHILD | WS_CLIPSIBLINGS | WS_CLIPCHILDREN,
                0,
                0,
                16,
                16,
                Some(HWND(parent_hwnd as *mut c_void)),
                None,
                Some(hi),
                None,
            )
        }
        .map_err(|e| format!("cannot create video window: {e}"))?;

        let opts = [
            "--no-video-title-show",
            "--no-osd",
            "--file-caching=100",
            "--input-fast-seek",
            "--quiet",
        ];
        let c_opts: Vec<CString> = opts.iter().map(|o| CString::new(*o).unwrap()).collect();
        let ptrs: Vec<*const c_char> = c_opts.iter().map(|c| c.as_ptr()).collect();

        let inst = unsafe { (api.new)(ptrs.len() as c_int, ptrs.as_ptr()) };
        if inst.is_null() {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return Err("libvlc_new failed".into());
        }
        let mp = unsafe { (api.mp_new)(inst) };
        if mp.is_null() {
            unsafe {
                (api.release)(inst);
                let _ = DestroyWindow(hwnd);
            }
            return Err("libvlc_media_player_new failed".into());
        }

        let sh = Arc::new(Shared {
            frame_ready: AtomicBool::new(false),
            ended: AtomicBool::new(false),
            errored: AtomicBool::new(false),
            last_seek_target: AtomicI64::new(-1),
            last_seek_tick: AtomicI64::new(-1),
            volume: AtomicI32::new(100),
            muted: AtomicBool::new(false),
            audio_applied: AtomicBool::new(false),
        });
        unsafe {
            (api.mp_set_hwnd)(mp, hwnd.0);
            (api.video_set_mouse_input)(mp, 0);
            (api.video_set_key_input)(mp, 0);
            let em = (api.mp_event_manager)(mp);
            for ev in EVENTS {
                (api.ev_attach)(em, ev, on_event, Arc::as_ptr(&sh) as *mut c_void);
            }
        }
        Ok(Player { api, hwnd, inst, mp, sh, lock: Mutex::new(()) })
    }

    /// Starts playing `path` from 0. The window is hidden until the caller shows it after `frame_ready()`
    /// (the surface would otherwise still hold the last frame of the previous video).
    pub fn load(&mut self, path: &Path) -> Result<(), String> {
        let _g = self.lock.lock().unwrap();
        let a = self.api;
        let s = path.to_str().ok_or("path is not valid unicode")?;
        let c = CString::new(s).map_err(|_| "path contains NUL".to_string())?;
        unsafe {
            let st = (a.mp_get_state)(self.mp);
            if st != ST_NOTHING && st != ST_STOPPED {
                (a.mp_stop)(self.mp);
            }
            let _ = ShowWindow(self.hwnd, SW_HIDE);
            self.sh.frame_ready.store(false, Ordering::Release);
            self.sh.ended.store(false, Ordering::Release);
            self.sh.errored.store(false, Ordering::Release);
            self.sh.audio_applied.store(false, Ordering::Release);
            self.sh.last_seek_target.store(-1, Ordering::Relaxed);
            self.sh.last_seek_tick.store(-1, Ordering::Relaxed);

            let media = (a.media_new_path)(self.inst, c.as_ptr());
            if media.is_null() {
                return Err("libvlc_media_new_path failed".into());
            }
            (a.mp_set_media)(self.mp, media);
            (a.media_release)(media);
            if (a.mp_play)(self.mp) != 0 {
                return Err("libvlc_media_player_play failed".into());
            }
            (a.audio_set_volume)(self.mp, self.sh.volume.load(Ordering::Relaxed));
            (a.audio_set_mute)(self.mp, self.sh.muted.load(Ordering::Relaxed) as c_int);
        }
        Ok(())
    }

    /// Child window position/size in parent client pixels.
    pub fn set_rect(&self, x: i32, y: i32, w: i32, h: i32) {
        unsafe {
            let _ = SetWindowPos(self.hwnd, None, x, y, w.max(1), h.max(1), SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }

    pub fn show(&self, visible: bool) {
        unsafe {
            let _ = ShowWindow(self.hwnd, if visible { SW_SHOWNA } else { SW_HIDE });
        }
    }

    /// True once the first video frame (or audio start) of the loaded media has been produced.
    pub fn frame_ready(&self) -> bool {
        let r = self.sh.frame_ready.load(Ordering::Acquire);
        // the audio output exists by now: re-apply volume/mute once (libvlc drops them while it does not)
        if r && !self.sh.audio_applied.swap(true, Ordering::AcqRel) {
            unsafe {
                (self.api.audio_set_volume)(self.mp, self.sh.volume.load(Ordering::Relaxed));
                (self.api.audio_set_mute)(self.mp, self.sh.muted.load(Ordering::Relaxed) as c_int);
            }
        }
        r
    }

    /// False for audio-only files (and before the video output exists).
    pub fn has_video(&self) -> bool {
        unsafe { (self.api.mp_has_vout)(self.mp) > 0 }
    }

    pub fn is_playing(&self) -> bool {
        unsafe { (self.api.mp_is_playing)(self.mp) != 0 }
    }

    /// True after the media played to its end, or after a playback error.
    pub fn finished(&self) -> bool {
        self.sh.ended.load(Ordering::Acquire) || self.sh.errored.load(Ordering::Acquire)
    }

    pub fn toggle_pause(&self) {
        unsafe { (self.api.mp_set_pause)(self.mp, self.is_playing() as c_int) }
    }

    /// Player clock. Stale for a short moment after a seek, see the module docs.
    pub fn time_ms(&self) -> i64 {
        unsafe { (self.api.mp_get_time)(self.mp).max(0) }
    }

    pub fn length_ms(&self) -> i64 {
        unsafe { (self.api.mp_get_length)(self.mp).max(0) }
    }

    /// Fast (keyframe) seek, clamped to [0, length-300]. Records the target for `last_seek_target`.
    pub fn seek_to_ms(&self, ms: i64) {
        let len = self.length_ms();
        if len <= 0 {
            return;
        }
        let t = ms.clamp(0, (len - 300).max(0));
        self.sh.last_seek_target.store(t, Ordering::Relaxed);
        self.sh.last_seek_tick.store(now_ms(), Ordering::Relaxed);
        unsafe { (self.api.mp_set_time)(self.mp, t) }
    }

    /// Target of the most recent seek (None if no seek since `load`). Show this instead of `time_ms()`
    /// while `recently_sought()`, and chain relative seeks from it.
    pub fn last_seek_target(&self) -> Option<i64> {
        let t = self.sh.last_seek_target.load(Ordering::Relaxed);
        (t >= 0).then_some(t)
    }

    /// A seek was issued within the last 350 ms (the clock still reports the old time).
    pub fn recently_sought(&self) -> bool {
        let tick = self.sh.last_seek_tick.load(Ordering::Relaxed);
        tick >= 0 && now_ms() - tick < 350
    }

    /// Last volume set (the audio output may not exist yet, so libvlc cannot be asked reliably).
    pub fn volume(&self) -> i32 {
        self.sh.volume.load(Ordering::Relaxed)
    }

    pub fn set_volume(&self, v: i32) {
        let v = v.clamp(0, 150);
        self.sh.volume.store(v, Ordering::Relaxed);
        unsafe {
            (self.api.audio_set_volume)(self.mp, v);
        }
    }

    pub fn muted(&self) -> bool {
        self.sh.muted.load(Ordering::Relaxed)
    }

    pub fn set_muted(&self, m: bool) {
        self.sh.muted.store(m, Ordering::Relaxed);
        unsafe { (self.api.audio_set_mute)(self.mp, m as c_int) }
    }

    /// Stops playback and hides the video window.
    pub fn stop(&self) {
        let _g = self.lock.lock().unwrap();
        unsafe {
            let st = (self.api.mp_get_state)(self.mp);
            if st != ST_NOTHING && st != ST_STOPPED {
                (self.api.mp_stop)(self.mp);
            }
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        self.sh.frame_ready.store(false, Ordering::Release);
    }

    /// Native video size, once known.
    pub fn video_size(&self) -> Option<(u32, u32)> {
        let (mut w, mut h): (c_uint, c_uint) = (0, 0);
        let rc = unsafe { (self.api.video_get_size)(self.mp, 0, &mut w, &mut h) };
        (rc == 0 && w > 0 && h > 0).then_some((w, h))
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let a = self.api;
        unsafe {
            // detach first so no callback can run against freed shared state
            let em = (a.mp_event_manager)(self.mp);
            for ev in EVENTS {
                (a.ev_detach)(em, ev, on_event, Arc::as_ptr(&self.sh) as *mut c_void);
            }
            let st = (a.mp_get_state)(self.mp);
            if st != ST_NOTHING && st != ST_STOPPED {
                (a.mp_stop)(self.mp);
            }
            (a.mp_release)(self.mp);
            (a.release)(self.inst);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}
