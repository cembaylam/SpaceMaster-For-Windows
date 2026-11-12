use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-changed=assets/app.manifest");
    println!("cargo:rerun-if-env-changed=PROFILE");

    // Release builds always start elevated (full access to every folder, no restart needed). Debug builds stay
    // "asInvoker" so `cargo test` can launch the UI self-test without a UAC prompt.
    let release = std::env::var("PROFILE").map(|p| p == "release").unwrap_or(false);
    let manifest = std::fs::read_to_string("assets/app.manifest").expect("read manifest");
    let manifest = if release {
        manifest.replace("level=\"asInvoker\"", "level=\"requireAdministrator\"")
    } else {
        manifest
    };

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let manifest_path = out.join("app.manifest");
    std::fs::write(&manifest_path, manifest).unwrap();
    let root = std::env::current_dir().unwrap();
    let slash = |p: PathBuf| p.to_string_lossy().replace('\\', "/");
    let rc = std::fs::read_to_string("assets/app.rc")
        .expect("read rc")
        .replace("\"app.ico\"", &format!("\"{}\"", slash(root.join("assets/app.ico"))))
        .replace("\"app.manifest\"", &format!("\"{}\"", slash(manifest_path)));
    let rc_path = out.join("app.rc");
    std::fs::write(&rc_path, rc).unwrap();

    // Icon, version info and the application manifest (per-monitor DPI, long paths, UTF-8) go into the GUI exe only.
    let _ = embed_resource::compile_for(rc_path, ["spacemaster"], embed_resource::NONE);
}
