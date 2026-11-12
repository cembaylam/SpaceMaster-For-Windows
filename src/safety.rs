//! Whether removing an item is reasonable. "Safe" means only that deleting it won't damage Windows or the
//! user's account; whether the *content* matters is the user's call.

use crate::enumerate::ATTR_SYSTEM;
use crate::paths;
use std::borrow::Cow;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Safe,
    Protected,
    /// The item itself can't be deleted, but it contains items that can.
    Mixed,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Safety {
    pub level: Level,
    pub reason: Cow<'static, str>,
}

pub const SAFE_REASON: &str = "Safe to delete. Deleting frees this space; check that you don't need the contents.";
pub const MIXED_REASON: &str = "Can't be deleted itself, but it contains items you can delete. Open it to find them.";

impl Safety {
    pub const SAFE: Safety = Safety { level: Level::Safe, reason: Cow::Borrowed(SAFE_REASON) };
    pub fn is_deletable(&self) -> bool {
        self.level == Level::Safe
    }
    fn protected(reason: &'static str) -> Safety {
        Safety { level: Level::Protected, reason: Cow::Borrowed(reason) }
    }
}

type Names = &'static [(&'static str, &'static str)];

/// Classification rules for one directory listing: one context per listing, O(#rules) per entry, no allocation.
#[derive(Clone, Debug)]
pub struct Context {
    blanket: Option<&'static str>,
    names: [Names; 2],
    /// Directories that are protected themselves but hold deletable items.
    mixed_all: bool,
    mixed_names: &'static [&'static str],
    never_mixed: &'static [&'static str],
    /// `OneDrive*` folders (cloud sync roots).
    onedrive: bool,
    /// `ntuser.*` registry hive files.
    ntuser: bool,
}

impl Context {
    const NONE: Context = Context {
        blanket: None,
        names: [&[], &[]],
        mixed_all: false,
        mixed_names: &[],
        never_mixed: &[],
        onedrive: false,
        ntuser: false,
    };
}

const STD_FOLDER: &str = "Standard Windows folder. Delete its contents instead.";
const LEGACY_LINK: &str = "Legacy compatibility link kept for old programs.";

static HOME_NAMES: Names = &[
    ("Desktop", STD_FOLDER),
    ("Documents", STD_FOLDER),
    ("Downloads", STD_FOLDER),
    ("Music", STD_FOLDER),
    ("Pictures", STD_FOLDER),
    ("Videos", STD_FOLDER),
    ("Favorites", STD_FOLDER),
    ("Links", STD_FOLDER),
    ("Contacts", STD_FOLDER),
    ("Searches", STD_FOLDER),
    ("Saved Games", STD_FOLDER),
    ("3D Objects", STD_FOLDER),
    ("AppData", "Your app data and settings. Delete items inside it instead."),
    (".ssh", "Holds your SSH keys. Deleting it can lock you out of servers."),
    (".gnupg", "Holds your GPG keys."),
    ("NTUSER.DAT", "Your user registry hive. Windows needs it to sign you in."),
    ("Application Data", LEGACY_LINK),
    ("Local Settings", LEGACY_LINK),
    ("My Documents", LEGACY_LINK),
    ("Cookies", LEGACY_LINK),
    ("NetHood", LEGACY_LINK),
    ("PrintHood", LEGACY_LINK),
    ("Recent", LEGACY_LINK),
    ("SendTo", LEGACY_LINK),
    ("Start Menu", LEGACY_LINK),
    ("Templates", LEGACY_LINK),
];
static HOME_NEVER_MIXED: &[&str] = &[
    ".ssh", ".gnupg", "NTUSER.DAT", "Application Data", "Local Settings", "My Documents", "Cookies", "NetHood",
    "PrintHood", "Recent", "SendTo", "Start Menu", "Templates",
];

static APPDATA_NAMES: Names = &[
    ("Local", "Standard app data folder. Delete items inside it instead."),
    ("Roaming", "Standard app data folder. Delete items inside it instead."),
    ("LocalLow", "Standard app data folder. Delete items inside it instead."),
];
static ROAMING_NAMES: Names =
    &[("Microsoft", "Windows and Office user data. Delete items inside it instead.")];
static ROAMING_MS_NAMES: Names = &[
    ("Credentials", "Holds your saved Windows credentials."),
    ("Protect", "Holds the keys that decrypt your saved passwords."),
    ("Crypto", "Holds your encryption keys and certificates."),
    ("SystemCertificates", "Holds your certificates."),
    ("Vault", "Holds your saved passwords."),
];
static LOCAL_NAMES: Names = &[
    ("Microsoft", "Windows and Office user data. Delete items inside it instead."),
    ("Packages", "Data for Microsoft Store apps. Delete individual app folders instead."),
];
static LOCAL_MS_NAMES: Names = &[
    ("Credentials", "Holds your saved Windows credentials."),
    ("Vault", "Holds your saved passwords."),
    ("Windows", "Windows shell and cache data. Delete items inside it instead."),
];

static ROOT_COMMON: Names = &[
    ("$Recycle.Bin", "The Recycle Bin. Empty it from the desktop Recycle Bin instead."),
    ("System Volume Information", "Restore points and indexing data kept by Windows."),
    ("pagefile.sys", "Windows virtual memory. Change it in System settings."),
    ("hiberfil.sys", "Hibernation file. Disable hibernation with powercfg to remove it."),
    ("swapfile.sys", "Windows virtual memory for apps."),
    ("DumpStack.log.tmp", "Windows crash diagnostics."),
];
static ROOT_SYSTEM: Names = &[
    ("Windows", "The Windows operating system."),
    ("Boot", "Windows boot files."),
    ("EFI", "Windows boot files."),
    ("Recovery", "Windows recovery environment."),
    ("bootmgr", "Windows boot manager."),
    ("BOOTNXT", "Windows boot files."),
    ("Documents and Settings", LEGACY_LINK),
    ("$WinREAgent", "Windows update recovery data."),
    ("Config.Msi", "Windows Installer rollback data."),
    ("Windows.old", "Previous Windows installation. Remove it with Storage settings > Temporary files."),
    ("Program Files", "Installed programs. Uninstall them from Settings > Apps instead."),
    ("Program Files (x86)", "Installed programs. Uninstall them from Settings > Apps instead."),
    ("Users", "User accounts folder."),
    ("ProgramData", "Shared app data. Delete items inside it instead."),
];
static ROOT_SYSTEM_MIXED: &[&str] = &["Users", "ProgramData"];
static ROOT_SYSTEM_NEVER_MIXED: &[&str] = &[];
static USERS_NAMES: Names = &[
    ("Default", "Template for new user accounts."),
    ("Default User", "Template for new user accounts."),
    ("All Users", "Shared profile link."),
    ("Public", "Shared files folder."),
    ("desktop.ini", "Folder settings."),
];
static PROGRAMDATA_NAMES: Names = &[
    ("Microsoft", "Shared Windows and Office data."),
    ("Package Cache", "Installer cache needed to repair or uninstall programs."),
    ("USOShared", "Windows Update data."),
    ("USOPrivate", "Windows Update data."),
    ("regid.1991-06.com.microsoft", "Software licensing data."),
];
static WINDOWS_EXCEPTIONS: &[&str] = &["Temp", "Logs", "SoftwareDistribution"];

/// Probe: can entries be removed from this directory without administrator rights?
pub type WriteProbe = fn(&str) -> bool;

/// Classifies entries per directory.
pub struct SafetyClassifier {
    home: String,
    system_drive: String,
    windows: String,
    program_dirs: [String; 2],
    probe: WriteProbe,
}

impl SafetyClassifier {
    /// Real environment: the current user's profile, `%SystemDrive%`, and a real access probe.
    pub fn new() -> Self {
        let sd = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        Self::with_layout(&paths::home_dir(), &sd, probe_can_write)
    }

    /// Explicit layout (tests): home folder, system drive such as `C:`, and the write probe.
    pub fn with_layout(home: &str, system_drive: &str, probe: WriteProbe) -> Self {
        let sd = system_drive.trim_end_matches('\\').to_string();
        SafetyClassifier {
            home: home.trim_end_matches('\\').to_string(),
            windows: format!("{sd}\\Windows"),
            program_dirs: [format!("{sd}\\Program Files"), format!("{sd}\\Program Files (x86)")],
            system_drive: sd,
            probe,
        }
    }

    pub fn home(&self) -> &str {
        &self.home
    }

    fn sd(&self, rest: &str) -> String {
        format!("{}\\{}", self.system_drive, rest)
    }

    pub fn context(&self, dir: &str) -> Context {
        let dir = dir.trim_end_matches('\\');
        // "This PC": the drives themselves.
        if dir.is_empty() {
            return Context {
                blanket: Some("A drive can't be deleted. Open it to find items you can delete."),
                mixed_all: true,
                ..Context::NONE
            };
        }
        let is_root = dir.len() == 2 && dir.ends_with(':');
        if is_root {
            return if paths::eq_ci(dir, &self.system_drive) {
                Context {
                    names: [ROOT_COMMON, ROOT_SYSTEM],
                    mixed_names: ROOT_SYSTEM_MIXED,
                    never_mixed: ROOT_SYSTEM_NEVER_MIXED,
                    ..Context::NONE
                }
            } else {
                Context { names: [ROOT_COMMON, &[]], ..Context::NONE }
            };
        }
        // Whole trees owned by Windows.
        if paths::is_within(dir, &self.windows) {
            let rel = dir[self.windows.len()..].trim_start_matches('\\');
            let first = rel.split('\\').next().unwrap_or("");
            if rel.is_empty() {
                return Context {
                    blanket: Some("System file protected by Windows."),
                    mixed_names: WINDOWS_EXCEPTIONS,
                    ..Context::NONE
                };
            }
            if !WINDOWS_EXCEPTIONS.iter().any(|e| e.eq_ignore_ascii_case(first)) {
                return Context { blanket: Some("System file protected by Windows."), ..Context::NONE };
            }
            // Temp / Logs / SoftwareDistribution: fall through to the permission probe.
        } else {
            for t in ["Boot", "EFI", "Recovery", "$WinREAgent", "Config.Msi", "Windows.old", "Documents and Settings"] {
                if paths::is_within(dir, &self.sd(t)) {
                    return Context { blanket: Some("System file protected by Windows."), ..Context::NONE };
                }
            }
            for p in &self.program_dirs {
                if paths::is_within(dir, p) {
                    return Context {
                        blanket: Some("Installed program. Uninstall it from Settings > Apps instead."),
                        ..Context::NONE
                    };
                }
            }
            // $Recycle.Bin and System Volume Information on any drive.
            let after_drive = dir.get(3..).unwrap_or("");
            let first = after_drive.split('\\').next().unwrap_or("");
            if dir.len() > 3
                && (first.eq_ignore_ascii_case("$Recycle.Bin") || first.eq_ignore_ascii_case("System Volume Information"))
            {
                return Context { blanket: Some("System file protected by Windows."), ..Context::NONE };
            }
        }
        if paths::eq_ci(dir, &self.sd("Users")) {
            return Context {
                blanket: Some("User accounts folder."),
                names: [USERS_NAMES, &[]],
                mixed_all: true,
                never_mixed: &["Default", "Default User", "All Users", "Public", "desktop.ini"],
                ..Context::NONE
            };
        }
        if paths::eq_ci(dir, &self.sd("ProgramData")) {
            return Context {
                names: [PROGRAMDATA_NAMES, &[]],
                mixed_names: &["Microsoft"],
                never_mixed: &["Package Cache", "USOShared", "USOPrivate", "regid.1991-06.com.microsoft"],
                ..Context::NONE
            };
        }
        if paths::eq_ci(dir, &self.home) {
            return Context {
                names: [HOME_NAMES, &[]],
                mixed_all: true,
                never_mixed: HOME_NEVER_MIXED,
                onedrive: true,
                ntuser: true,
                ..Context::NONE
            };
        }
        let app = format!("{}\\AppData", self.home);
        if paths::is_within(dir, &app) {
            let rel = dir[app.len()..].trim_start_matches('\\');
            let ctx = match rel.to_ascii_lowercase().as_str() {
                "" => Some(Context { names: [APPDATA_NAMES, &[]], mixed_all: true, ..Context::NONE }),
                "roaming" => Some(Context {
                    names: [ROAMING_NAMES, &[]],
                    mixed_names: &["Microsoft"],
                    ..Context::NONE
                }),
                "roaming\\microsoft" => Some(Context { names: [ROAMING_MS_NAMES, &[]], ..Context::NONE }),
                "local" => Some(Context {
                    names: [LOCAL_NAMES, &[]],
                    mixed_names: &["Microsoft", "Packages"],
                    ..Context::NONE
                }),
                "local\\microsoft" => Some(Context {
                    names: [LOCAL_MS_NAMES, &[]],
                    mixed_names: &["Windows"],
                    never_mixed: &["Credentials", "Vault"],
                    ..Context::NONE
                }),
                _ => None,
            };
            if let Some(c) = ctx {
                return c;
            }
        }
        // Directories we can't write to: entries can't be removed without administrator rights.
        if !(self.probe)(dir) {
            return Context {
                blanket: Some("You don't have permission to delete items here. Restart as administrator to try."),
                ..Context::NONE
            };
        }
        Context::NONE
    }

    pub fn classify(&self, name: &str, attrs: u32, is_dir: bool, ctx: &Context) -> Safety {
        if attrs & ATTR_SYSTEM != 0 {
            return Safety::protected("Windows system item.");
        }
        let in_never = ctx.never_mixed.iter().any(|n| n.eq_ignore_ascii_case(name));
        let mixed = is_dir
            && !in_never
            && (ctx.mixed_all || ctx.mixed_names.iter().any(|n| n.eq_ignore_ascii_case(name)));
        if let Some(b) = ctx.blanket {
            return if mixed { mixed_safety(MIXED_REASON.into()) } else { Safety::protected(b) };
        }
        for list in &ctx.names {
            if let Some((_, r)) = list.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
                return if mixed {
                    mixed_safety(Cow::Owned(format!("{r} It contains items you can delete; open it to find them.")))
                } else {
                    Safety::protected(r)
                };
            }
        }
        if ctx.onedrive && name.len() >= 8 && name[..8].eq_ignore_ascii_case("OneDrive") {
            return Safety::protected("Your OneDrive folder. Deleting here deletes from the cloud, too.");
        }
        if ctx.ntuser && name.len() > 7 && name[..7].eq_ignore_ascii_case("ntuser.") {
            return Safety::protected("Your user registry hive data.");
        }
        Safety::SAFE
    }

    /// Classification of an arbitrary path (info panel and the final check before deleting).
    pub fn classify_path(&self, path: &str, attrs: u32, is_dir: bool) -> Safety {
        let path = path.trim_end_matches('\\');
        if path.is_empty() {
            return Safety::protected("This PC.");
        }
        if path.len() == 2 {
            return Safety::protected("A drive can't be deleted.");
        }
        if paths::eq_ci(path, &self.home) {
            return Safety::protected("Your user folder.");
        }
        let parent = paths::parent(path).unwrap_or_default();
        let ctx = self.context(&parent);
        self.classify(paths::file_name(path), attrs, is_dir, &ctx)
    }
}

impl Default for SafetyClassifier {
    fn default() -> Self {
        Self::new()
    }
}

fn mixed_safety(reason: Cow<'static, str>) -> Safety {
    Safety { level: Level::Mixed, reason }
}

/// Real probe: can we open the directory with permission to add and delete entries?
fn probe_can_write(dir: &str) -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    let w = paths::to_wide_nt(dir);
    // FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY
    let h = unsafe {
        CreateFileW(
            PCWSTR(w.as_ptr()),
            0x0002 | 0x0004,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    };
    match h {
        Ok(h) => {
            unsafe {
                let _ = CloseHandle(h);
            }
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c() -> SafetyClassifier {
        SafetyClassifier::with_layout("C:\\Users\\test", "C:", |_| true)
    }

    #[test]
    fn windows_tree_is_protected() {
        let c = c();
        assert_eq!(c.classify_path("C:\\Windows", 0, true).level, Level::Protected);
        assert_eq!(c.classify_path("C:\\Windows\\System32", 0, true).level, Level::Protected);
        assert_eq!(c.classify_path("C:\\windows\\system32\\cmd.exe", 0, false).level, Level::Protected);
        assert_eq!(c.classify_path("C:\\Program Files\\Git", 0, true).level, Level::Protected);
        assert_eq!(c.classify_path("C:\\Program Files (x86)", 0, true).level, Level::Protected);
        assert_eq!(c.classify_path("C:\\System Volume Information", 0, true).level, Level::Protected);
        assert_eq!(c.classify_path("D:\\$RECYCLE.BIN", 0, true).level, Level::Protected);
        assert_eq!(c.classify_path("C:\\pagefile.sys", 0, false).level, Level::Protected);
        assert_eq!(c.classify_path("C:\\Windows.old", 0, true).level, Level::Protected);
    }

    #[test]
    fn windows_temp_is_not_blanket_protected() {
        let c = c();
        assert_eq!(c.classify_path("C:\\Windows\\Temp\\junk.tmp", 0, false).level, Level::Safe);
        let ctx = c.context("C:\\Windows");
        assert_eq!(c.classify("Temp", 0, true, &ctx).level, Level::Mixed);
        assert_eq!(c.classify("System32", 0, true, &ctx).level, Level::Protected);
    }

    #[test]
    fn user_folders() {
        let c = c();
        assert_eq!(c.classify_path("C:\\Users\\test", 0, true).level, Level::Protected);
        assert_eq!(c.classify_path("C:\\Users\\test\\Documents", 0, true).level, Level::Mixed);
        assert_eq!(c.classify_path("C:\\Users\\test\\.ssh", 0, true).level, Level::Protected);
        assert_eq!(c.classify_path("C:\\Users\\test\\NTUSER.DAT", 0, false).level, Level::Protected);
        assert_eq!(c.classify_path("C:\\Users\\test\\ntuser.dat.LOG1", 0, false).level, Level::Protected);
        assert_eq!(c.classify_path("C:\\Users\\test\\OneDrive - Contoso", 0, true).level, Level::Protected);
        assert_eq!(c.classify_path("C:\\Users\\test\\AppData", 0, true).level, Level::Mixed);
        assert_eq!(c.classify_path("C:\\Users\\test\\random-folder", 0, true).level, Level::Safe);
        assert_eq!(c.classify_path("C:\\Users\\test\\Documents\\report.docx", 0, false).level, Level::Safe);
    }

    #[test]
    fn appdata_rules() {
        let c = c();
        assert_eq!(c.classify_path("C:\\Users\\test\\AppData\\Roaming", 0, true).level, Level::Mixed);
        assert_eq!(c.classify_path("C:\\Users\\test\\AppData\\Roaming\\Microsoft", 0, true).level, Level::Mixed);
        assert_eq!(c.classify_path("C:\\Users\\test\\AppData\\Roaming\\Microsoft\\Protect", 0, true).level, Level::Protected);
        assert_eq!(c.classify_path("C:\\Users\\test\\AppData\\Roaming\\Microsoft\\Credentials", 0, true).level, Level::Protected);
        assert_eq!(c.classify_path("C:\\Users\\test\\AppData\\Local\\Packages", 0, true).level, Level::Mixed);
        assert_eq!(c.classify_path("C:\\Users\\test\\AppData\\Local\\Temp", 0, true).level, Level::Safe);
        assert_eq!(c.classify_path("C:\\Users\\test\\AppData\\Local\\Microsoft\\Vault", 0, true).level, Level::Protected);
        assert_eq!(c.classify_path("C:\\Users\\test\\AppData\\Local\\Google", 0, true).level, Level::Safe);
    }

    #[test]
    fn drives_and_users_are_mixed() {
        let c = c();
        let root = c.context("C:\\");
        assert_eq!(c.classify("Users", 0, true, &root).level, Level::Mixed);
        assert_eq!(c.classify("ProgramData", 0, true, &root).level, Level::Mixed);
        assert_eq!(c.classify("Windows", 0, true, &root).level, Level::Protected);
        assert_eq!(c.classify("Program Files", 0, true, &root).level, Level::Protected);
        assert_eq!(c.classify("Projeler", 0, true, &root).level, Level::Safe);
        let users = c.context("C:\\Users");
        assert_eq!(c.classify("test", 0, true, &users).level, Level::Mixed);
        assert_eq!(c.classify("Public", 0, true, &users).level, Level::Protected);
        let pc = c.context("");
        assert_eq!(c.classify("C:", 0, true, &pc).level, Level::Mixed);
        assert_eq!(c.classify_path("C:\\", 0, true).level, Level::Protected);
        // Other drives: only the Windows bookkeeping folders are protected.
        let d = c.context("D:\\");
        assert_eq!(c.classify("Games", 0, true, &d).level, Level::Safe);
        assert_eq!(c.classify("System Volume Information", 0, true, &d).level, Level::Protected);
    }

    #[test]
    fn system_attribute_is_protected() {
        let c = c();
        assert_eq!(c.classify_path("C:\\Projeler\\x.bin", ATTR_SYSTEM, false).level, Level::Protected);
    }

    #[test]
    fn no_write_permission_is_protected() {
        let c = SafetyClassifier::with_layout("C:\\Users\\test", "C:", |_| false);
        assert_eq!(c.classify_path("C:\\ProgramData\\Foo\\bar", 0, true).level, Level::Protected);
        assert_eq!(c.classify_path("C:\\Users\\test\\proj\\x", 0, true).level, Level::Protected);
    }
}
