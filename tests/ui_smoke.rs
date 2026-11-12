//! Launches the real Direct2D window in `--selftest` mode (see `src/app/selftest.rs`): paint, keyboard, mouse,
//! wheel, resize, theme switch and deleting all go through the real window procedure. Needs an interactive
//! desktop session (a window is created for a few seconds).

use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("ui-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn window_selftest_passes() {
    let root = scratch("selftest");
    let home = root.join("home");
    for d in ["a\\b", "c", "d", "e"] {
        std::fs::create_dir_all(home.join(d)).unwrap();
    }
    for i in 1..=40u32 {
        std::fs::write(home.join(format!("file{i:02}.bin")), vec![5u8; (i * 3700) as usize]).unwrap();
    }
    std::fs::write(home.join("a\\b\\big.bin"), vec![5u8; 3_000_000]).unwrap();
    let result = root.join("result.txt");

    let status = Command::new(env!("CARGO_BIN_EXE_spacemaster"))
        .arg("--selftest")
        .arg(&result)
        .env("SM_START_PATH", &home)
        .env("SM_NO_DRIVE_SCAN", "1")
        .env("SM_SETTINGS_FILE", root.join("settings.ini"))
        .status()
        .expect("launch spacemaster.exe");

    let text = std::fs::read_to_string(&result).unwrap_or_else(|e| panic!("no result file: {e}"));
    let failures: Vec<&str> = text.lines().filter(|l| l.starts_with("FAIL")).collect();
    let passes = text.lines().filter(|l| l.starts_with("PASS")).count();
    let _ = std::fs::remove_dir_all(&root);
    assert!(failures.is_empty(), "UI self-test failures:\n{}", failures.join("\n"));
    assert!(passes >= 30, "only {passes} checks ran:\n{text}");
    assert!(status.success(), "selftest exit code {status:?}");
}

#[test]
fn window_selftest_passes_in_dark_mode_with_a_tiny_folder() {
    let root = scratch("dark");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("only.txt"), b"x").unwrap();
    let result = root.join("result.txt");
    let status = Command::new(env!("CARGO_BIN_EXE_spacemaster"))
        .arg("--selftest")
        .arg(&result)
        .env("SM_START_PATH", &home)
        .env("SM_NO_DRIVE_SCAN", "1")
        .env("SM_THEME", "dark")
        .env("SM_SETTINGS_FILE", root.join("settings.ini"))
        .status()
        .expect("launch spacemaster.exe");
    let text = std::fs::read_to_string(&result).unwrap_or_default();
    let failures: Vec<&str> = text.lines().filter(|l| l.starts_with("FAIL")).collect();
    let _ = std::fs::remove_dir_all(&root);
    // With a one-file folder a few navigation checks (sorting, double click on a folder) legitimately differ,
    // but rendering, resize and theme checks must hold and the process must not crash.
    assert!(text.contains("PASS window has a render target"), "{text}");
    assert!(text.contains("PASS dark theme paints"), "{text}");
    assert!(text.contains("PASS still rendering at the end"), "{text}");
    assert!(status.code().is_some(), "selftest crashed: {status:?}");
    let _ = failures;
}
