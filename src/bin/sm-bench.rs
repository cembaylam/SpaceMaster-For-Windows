//! Scanner benchmark: `sm-bench [DIR] [--threads N] [--runs N]`
use spacemaster::{format, paths, scanner::Scanner};
use std::time::{Duration, Instant};

fn main() {
    let mut dir = paths::home_dir();
    let mut threads = Scanner::default_threads();
    let mut runs = 3;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--threads" => threads = args.next().and_then(|v| v.parse().ok()).unwrap_or(threads),
            "--phases" => { phases(&paths::normalize(&args.next().unwrap_or_default(), &dir).unwrap_or(dir.clone())); return; }
            "--stress" => { stress(args.next().and_then(|v| v.parse().ok()).unwrap_or(100_000)); return; }
            "--runs" => runs = args.next().and_then(|v| v.parse().ok()).unwrap_or(runs),
            other => dir = paths::normalize(other, &dir).unwrap_or(dir),
        }
    }
    println!("scanning {dir} with {threads} threads, {runs} runs");
    let mut best = f64::MAX;
    for run in 1..=runs {
        let scanner = Scanner::new(threads);
        let node = scanner.node_for_path(&dir);
        let t = Instant::now();
        scanner.scan(&node);
        scanner.wait_idle(Duration::from_secs(3600));
        let secs = t.elapsed().as_secs_f64();
        best = best.min(secs);
        let items = scanner.items_scanned();
        println!(
            "run {run}: {:.3}s  {} items  {} dirs  {}  {:.0} items/s  ({} unreadable)",
            secs,
            format::count(items as i64),
            format::count(scanner.dirs_scanned() as i64),
            format::bytes(node.total_bytes()),
            items as f64 / secs,
            scanner.denied()
        );
    }
    println!("best: {best:.3}s");
}

/// `sm-bench --phases DIR`: single-threaded timing of open / list / close over real directories.
#[allow(dead_code)]
pub fn phases(dir: &str) {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Storage::FileSystem::*;
    let mut dirs = vec![dir.to_string()];
    let mut i = 0;
    while i < dirs.len() && dirs.len() < 40_000 {
        if let Ok(rd) = std::fs::read_dir(&dirs[i]) {
            for e in rd.flatten() {
                if e.file_type().map(|t| t.is_dir() && !t.is_symlink()).unwrap_or(false) {
                    dirs.push(e.path().to_string_lossy().into_owned());
                }
            }
        }
        i += 1;
    }
    let wides: Vec<Vec<u16>> = dirs.iter().map(|d| paths::to_wide_nt(d)).collect();
    let mut buf = vec![0u64; 16 * 1024];
    let (mut t_open, mut t_list, mut t_close) = (0f64, 0f64, 0f64);
    let mut n = 0;
    for w in &wides {
        let t0 = Instant::now();
        let h = unsafe {
            CreateFileW(PCWSTR(w.as_ptr()), 1, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, None, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, None)
        };
        let t1 = Instant::now();
        let Ok(h) = h else { continue };
        let mut first = true;
        loop {
            let class = if first { FileFullDirectoryRestartInfo } else { FileFullDirectoryInfo };
            first = false;
            if unsafe { GetFileInformationByHandleEx(h, class, buf.as_mut_ptr().cast(), 128 * 1024) }.is_err() {
                break;
            }
        }
        let t2 = Instant::now();
        unsafe { let _ = CloseHandle(h); }
        let t3 = Instant::now();
        t_open += (t1 - t0).as_secs_f64();
        t_list += (t2 - t1).as_secs_f64();
        t_close += (t3 - t2).as_secs_f64();
        n += 1;
    }
    let us = |t: f64| t / n as f64 * 1e6;
    println!("{n} dirs: open {:.1}us  list {:.1}us  close {:.1}us per dir", us(t_open), us(t_list), us(t_close));
}

/// `sm-bench --stress N`: browse a folder with N files and N/50 sub-folders and time the UI-side work.
#[allow(dead_code)]
pub fn stress(n: usize) {
    use spacemaster::browser::{Browser, Key, KeyEvent};
    use spacemaster::model::Model;
    use spacemaster::safety::SafetyClassifier;
    use spacemaster::settings::Settings;
    let root = std::env::temp_dir().join(format!("sm-stress-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let big = home.join("big");
    std::fs::create_dir_all(&big).unwrap();
    let t = Instant::now();
    for i in 0..n {
        std::fs::write(big.join(format!("file-{i:07}.dat")), b"x").unwrap();
    }
    for d in 0..(n / 50).max(1) {
        let sub = big.join(format!("dir-{d:05}"));
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("a.bin"), vec![1u8; 10_000]).unwrap();
    }
    println!("created {n} files + {} folders in {:.1}s", (n / 50).max(1), t.elapsed().as_secs_f64());
    let home_s = home.to_string_lossy().into_owned();
    let model = Model::with(Scanner::default_threads(), SafetyClassifier::with_layout(&home_s, "C:", |_| true));
    model.scan_drive.store(false, std::sync::atomic::Ordering::Relaxed);
    let mut b = Browser::new(model, Settings::memory());
    b.visible_rows = 40;
    b.model.scanner.wait_idle(Duration::from_secs(60));
    let t = Instant::now();
    b.cd(&big.to_string_lossy(), None, true);
    println!("cd into the big folder: {:.1} ms ({} rows)", t.elapsed().as_secs_f64() * 1e3, b.current.borrow().rows.len());
    b.model.scanner.wait_idle(Duration::from_secs(60));
    let t = Instant::now();
    b.refresh_visible();
    println!("rebuild (sort+classify+filter): {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
    let t = Instant::now();
    for _ in 0..200 {
        b.handle_key(KeyEvent::new(Key::Down));
    }
    println!("200 cursor moves (each updates preview): {:.2} ms/move", t.elapsed().as_secs_f64() * 1e3 / 200.0);
    let t = Instant::now();
    b.handle_key(KeyEvent::ch('/'));
    for c in "dir-0".chars() {
        b.handle_key(KeyEvent::ch(c));
    }
    println!("typing a filter (5 keystrokes): {:.1} ms total", t.elapsed().as_secs_f64() * 1e3);
    drop(b);
    let _ = std::fs::remove_dir_all(&root);
}
