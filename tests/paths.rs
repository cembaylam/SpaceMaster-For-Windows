use spacemaster::paths::*;
use std::cmp::Ordering;

#[test]
fn join_and_parent() {
    assert_eq!(join("", "C:"), r"C:\");
    assert_eq!(join(r"C:\", "Users"), r"C:\Users");
    assert_eq!(join(r"C:\Users", "me"), r"C:\Users\me");
    assert_eq!(parent(r"C:\Users\me").as_deref(), Some(r"C:\Users"));
    assert_eq!(parent(r"C:\Users").as_deref(), Some(r"C:\"));
    assert_eq!(parent(r"C:\").as_deref(), Some(""));
    assert_eq!(parent(""), None);
    assert_eq!(file_name(r"C:\Users\me"), "me");
    assert_eq!(file_name(r"C:\"), "C:");
    assert_eq!(file_name(r"C:\Users\me\"), "me");
    assert!(is_drive_root(r"D:\"));
    assert!(!is_drive_root(r"D:\x"));
    assert!(!is_drive_root("D:"));
}

#[test]
fn normalizing_user_input() {
    let home = r"C:\Users\me";
    assert_eq!(normalize("c:", home).as_deref(), Some(r"C:\"));
    assert_eq!(normalize("c:/users/me/", home).as_deref(), Some(r"C:\users\me"));
    assert_eq!(normalize("  \"D:\\Games\\\"  ", home).as_deref(), Some(r"D:\Games"));
    assert_eq!(normalize("~", home).as_deref(), Some(r"C:\Users\me"));
    assert_eq!(normalize(r"~\Downloads", home).as_deref(), Some(r"C:\Users\me\Downloads"));
    assert_eq!(normalize("~/Downloads", home).as_deref(), Some(r"C:\Users\me\Downloads"));
    assert_eq!(normalize(r"C:\a\..\b\.\c", home).as_deref(), Some(r"C:\b\c"));
    assert_eq!(normalize(r"C:\a\..\..\b", home).as_deref(), Some(r"C:\b"));
    assert_eq!(normalize(r"\\?\C:\Windows", home).as_deref(), Some(r"C:\Windows"));
    assert_eq!(normalize(r"C:\\double\\slash", home).as_deref(), Some(r"C:\double\slash"));
    assert_eq!(normalize(r"relative\path", home), None);
    assert_eq!(normalize(r"\\server\share", home), None);
    assert_eq!(normalize("", home), None);
    std::env::set_var("SM_TEST_VAR", r"E:\Data");
    assert_eq!(normalize(r"%SM_TEST_VAR%\x", home).as_deref(), Some(r"E:\Data\x"));
    // Unknown variables are left alone and therefore not a drive path.
    assert_eq!(normalize(r"%SM_NOT_SET_XYZ%\x", home), None);
}

#[test]
fn containment_is_case_insensitive_and_component_aware() {
    assert!(is_within(r"C:\Windows\System32", r"c:\windows"));
    assert!(is_within(r"C:\Windows", r"C:\Windows\"));
    assert!(!is_within(r"C:\WindowsApps", r"C:\Windows"));
    assert!(!is_within(r"C:\Win", r"C:\Windows"));
    assert!(is_within("C:\\Users\u{e9}\\x", "c:\\users\u{c9}"));
    assert!(eq_ci(r"C:\ABC", r"c:\abc"));
    assert!(!eq_ci(r"C:\ABC", r"c:\abd"));
}

#[test]
fn natural_order() {
    assert_eq!(natural_cmp("file2", "file10"), Ordering::Less);
    assert_eq!(natural_cmp("file10", "file2"), Ordering::Greater);
    assert_eq!(natural_cmp("File", "file"), Ordering::Equal);
    assert_eq!(natural_cmp("a", "b"), Ordering::Less);
    assert_eq!(natural_cmp("abc", "ab"), Ordering::Greater);
    assert_eq!(natural_cmp("x001", "x1"), Ordering::Equal);
    assert_eq!(natural_cmp("v1.2.10", "v1.2.9"), Ordering::Greater);
    let mut v = vec!["10", "9", "1", "100", "b", "a"];
    v.sort_by(|a, b| natural_cmp(a, b));
    assert_eq!(v, ["1", "9", "10", "100", "a", "b"]);
}

#[test]
fn wide_paths() {
    let w = to_wide_nt(r"C:\x");
    assert_eq!(String::from_utf16_lossy(&w[..w.len() - 1]), r"\\?\C:\x");
    assert_eq!(*w.last().unwrap(), 0);
    assert_eq!(to_wide("\u{e9}"), vec![0xE9, 0]);
}

#[test]
fn home_dir_is_a_drive_path() {
    let h = home_dir();
    assert!(h.len() >= 3 && h.as_bytes()[1] == b':', "{h}");
    assert!(std::path::Path::new(&h).is_dir());
}
