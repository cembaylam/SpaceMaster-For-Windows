use crate::enumerate::{self, DirBuf, ATTR_HIDDEN};
use crate::{paths, volumes};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    File,
    Directory,
    Symlink,
}

/// A directory entry as shown in the UI.
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub kind: Kind,
    /// Allocated bytes for files; for directories the UI fills this from the scan tree.
    pub size: i64,
    /// FILETIME of the last write.
    pub mtime: i64,
    pub attrs: u32,
    pub hidden: bool,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.kind == Kind::Directory
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListError {
    pub code: u32,
    pub message: String,
}

/// Lists one directory with all the attributes the UI needs. "" lists the drives.
pub fn list(path: &str) -> Result<Vec<Entry>, ListError> {
    if path.is_empty() {
        return Ok(volumes::list()
            .into_iter()
            .map(|v| Entry { name: v.drive(), kind: Kind::Directory, size: 0, mtime: 0, attrs: 0x10, hidden: false })
            .collect());
    }
    let wide = paths::to_wide_nt(path);
    let mut buf = DirBuf::new();
    let mut out: Vec<Entry> = Vec::with_capacity(256);
    enumerate::enumerate(&wide, &mut buf, |e| {
        let kind = if e.is_link() {
            Kind::Symlink
        } else if e.is_dir() {
            Kind::Directory
        } else {
            Kind::File
        };
        out.push(Entry {
            name: String::from_utf16_lossy(e.name),
            kind,
            size: e.alloc,
            mtime: e.mtime,
            attrs: e.attrs,
            hidden: e.attrs & ATTR_HIDDEN != 0,
        });
    })
    .map_err(|code| ListError { code, message: describe_error(code) })?;
    Ok(out)
}

pub fn describe_error(code: u32) -> String {
    match code {
        enumerate::ERROR_ACCESS_DENIED => {
            "No permission to read this folder.\nRestart SpaceMaster as administrator to see more.".into()
        }
        enumerate::ERROR_FILE_NOT_FOUND | enumerate::ERROR_PATH_NOT_FOUND => "This folder no longer exists.".into(),
        32 => "This folder is in use by another program.".into(),
        c => std::io::Error::from_raw_os_error(c as i32).to_string(),
    }
}
