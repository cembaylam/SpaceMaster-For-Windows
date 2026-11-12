//! Bulk directory enumeration: one `GetFileInformationByHandleEx(FileFullDirectoryInfo)` call returns the
//! name, attributes, size, *allocated* size and timestamps of hundreds of entries at once (a thin wrapper over
//! `NtQueryDirectoryFileEx`). This is the Windows counterpart of `getattrlistbulk(2)`.
//!
//! Sub-directories are opened *relative to their parent's handle* (`NtCreateFile` with `RootDirectory`), so the
//! kernel does not re-resolve and re-check the whole path for every one of the hundreds of thousands of folders.

use std::ffi::c_void;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FileFullDirectoryInfo, FileFullDirectoryRestartInfo, GetFileInformationByHandleEx,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FULL_DIR_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};

pub const ATTR_READONLY: u32 = 0x1;
pub const ATTR_HIDDEN: u32 = 0x2;
pub const ATTR_SYSTEM: u32 = 0x4;
pub const ATTR_DIRECTORY: u32 = 0x10;
pub const ATTR_OFFLINE: u32 = 0x1000;
pub const ATTR_REPARSE_POINT: u32 = 0x400;
pub const ATTR_RECALL_ON_OPEN: u32 = 0x40000;
pub const ATTR_RECALL_ON_DATA_ACCESS: u32 = 0x400000;

pub const REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
pub const REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;

pub const ERROR_ACCESS_DENIED: u32 = 5;
pub const ERROR_FILE_NOT_FOUND: u32 = 2;
pub const ERROR_PATH_NOT_FOUND: u32 = 3;

/// One directory entry as reported by the file system.
pub struct RawEntry<'a> {
    pub name: &'a [u16],
    pub attrs: u32,
    pub size: i64,
    /// Bytes allocated on disk (cluster rounded; 0 for cloud placeholders and tiny MFT-resident files).
    pub alloc: i64,
    /// FILETIME of the last write.
    pub mtime: i64,
    /// Valid when `attrs` has `ATTR_REPARSE_POINT`.
    pub reparse_tag: u32,
}

impl RawEntry<'_> {
    #[inline]
    pub fn is_dir(&self) -> bool {
        self.attrs & ATTR_DIRECTORY != 0
    }
    /// A symlink or junction: never followed, so no directory is counted twice.
    #[inline]
    pub fn is_link(&self) -> bool {
        self.attrs & ATTR_REPARSE_POINT != 0
            && (self.reparse_tag == REPARSE_TAG_SYMLINK || self.reparse_tag == REPARSE_TAG_MOUNT_POINT)
    }
    /// Cloud placeholder whose content is not local: listing it could trigger a download.
    #[inline]
    pub fn is_dataless_dir(&self) -> bool {
        self.attrs & (ATTR_RECALL_ON_OPEN | ATTR_RECALL_ON_DATA_ACCESS | ATTR_OFFLINE) != 0
    }
}

const BUF_BYTES: usize = 128 * 1024;

/// Reusable, 8-byte aligned listing buffer (one per worker thread).
pub struct DirBuf {
    mem: Vec<u64>,
}

impl DirBuf {
    pub fn new() -> Self {
        DirBuf { mem: vec![0u64; BUF_BYTES / 8] }
    }
}

impl Default for DirBuf {
    fn default() -> Self {
        Self::new()
    }
}

/// An open directory; closed on drop.
pub struct DirHandle(HANDLE);

// A directory handle is just a kernel object reference; it can be used and closed from any thread.
unsafe impl Send for DirHandle {}
unsafe impl Sync for DirHandle {}

impl Drop for DirHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *const u16,
}

#[repr(C)]
struct ObjectAttributes {
    length: u32,
    root_directory: HANDLE,
    object_name: *const UnicodeString,
    attributes: u32,
    security_descriptor: *const c_void,
    security_quality_of_service: *const c_void,
}

#[repr(C)]
struct IoStatusBlock {
    status: isize,
    information: usize,
}

#[link(name = "ntdll")]
extern "system" {
    fn NtCreateFile(
        file_handle: *mut HANDLE,
        desired_access: u32,
        object_attributes: *const ObjectAttributes,
        io_status_block: *mut IoStatusBlock,
        allocation_size: *const i64,
        file_attributes: u32,
        share_access: u32,
        create_disposition: u32,
        create_options: u32,
        ea_buffer: *const c_void,
        ea_length: u32,
    ) -> i32;
    fn RtlNtStatusToDosError(status: i32) -> u32;
}

/// Opens a directory by absolute path (`\\?\`-prefixed, NUL terminated UTF-16).
pub fn open_path(path: &[u16]) -> Result<DirHandle, u32> {
    debug_assert_eq!(path.last(), Some(&0));
    let h = unsafe {
        CreateFileW(
            PCWSTR(path.as_ptr()),
            0x0001, // FILE_LIST_DIRECTORY
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    };
    match h {
        Ok(h) if h != INVALID_HANDLE_VALUE => Ok(DirHandle(h)),
        _ => Err(unsafe { GetLastError().0 }),
    }
}

/// Opens the sub-directory `name` (UTF-16, no NUL) relative to an already open directory.
pub fn open_relative(parent: &DirHandle, name: &[u16]) -> Result<DirHandle, u32> {
    let us = UnicodeString {
        length: (name.len() * 2) as u16,
        maximum_length: (name.len() * 2) as u16,
        buffer: name.as_ptr(),
    };
    let oa = ObjectAttributes {
        length: std::mem::size_of::<ObjectAttributes>() as u32,
        root_directory: parent.0,
        object_name: &us,
        attributes: 0x40, // OBJ_CASE_INSENSITIVE
        security_descriptor: std::ptr::null(),
        security_quality_of_service: std::ptr::null(),
    };
    let mut iosb = IoStatusBlock { status: 0, information: 0 };
    let mut h = HANDLE::default();
    let status = unsafe {
        NtCreateFile(
            &mut h,
            0x0001 | 0x0010_0000, // FILE_LIST_DIRECTORY | SYNCHRONIZE
            &oa,
            &mut iosb,
            std::ptr::null(),
            0,
            7, // FILE_SHARE_READ | WRITE | DELETE
            1, // FILE_OPEN
            0x1 | 0x20 | 0x4000 | 0x20_0000, // DIRECTORY_FILE | SYNCHRONOUS_IO_NONALERT | OPEN_FOR_BACKUP_INTENT | OPEN_REPARSE_POINT
            std::ptr::null(),
            0,
        )
    };
    if status >= 0 {
        Ok(DirHandle(h))
    } else {
        Err(unsafe { RtlNtStatusToDosError(status) })
    }
}

/// Lists an open directory. Entries are reported to `f`; `.` and `..` are skipped.
pub fn list_handle(dir: &DirHandle, buf: &mut DirBuf, mut f: impl FnMut(&RawEntry)) -> Result<(), u32> {
    let mut first = true;
    loop {
        let class = if first { FileFullDirectoryRestartInfo } else { FileFullDirectoryInfo };
        first = false;
        let r = unsafe { GetFileInformationByHandleEx(dir.0, class, buf.mem.as_mut_ptr().cast(), BUF_BYTES as u32) };
        if r.is_err() {
            let e = unsafe { GetLastError().0 };
            // ERROR_NO_MORE_FILES (18) ends the listing normally.
            return if e == 18 { Ok(()) } else { Err(e) };
        }
        let base = buf.mem.as_ptr().cast::<u8>();
        let mut off = 0usize;
        loop {
            // SAFETY: the kernel filled the buffer with a chain of FILE_FULL_DIR_INFO records.
            let info = unsafe { &*(base.add(off) as *const FILE_FULL_DIR_INFO) };
            let name_len = (info.FileNameLength / 2) as usize;
            let name = unsafe { std::slice::from_raw_parts(info.FileName.as_ptr(), name_len) };
            let dot = (name_len == 1 && name[0] == b'.' as u16)
                || (name_len == 2 && name[0] == b'.' as u16 && name[1] == b'.' as u16);
            if !dot {
                f(&RawEntry {
                    name,
                    attrs: info.FileAttributes,
                    size: info.EndOfFile,
                    alloc: info.AllocationSize,
                    mtime: info.LastWriteTime,
                    reparse_tag: info.EaSize,
                });
            }
            if info.NextEntryOffset == 0 {
                // NTFS may return fewer records than fit in the buffer, so only ERROR_NO_MORE_FILES
                // (next call) is a reliable end of the listing.
                break;
            }
            off += info.NextEntryOffset as usize;
        }
    }
}

/// Lists `path` (NUL terminated UTF-16, `\\?\` prefixed). Returns the Win32 error code when the
/// directory cannot be opened or read.
pub fn enumerate(path: &[u16], buf: &mut DirBuf, f: impl FnMut(&RawEntry)) -> Result<(), u32> {
    let h = open_path(path)?;
    list_handle(&h, buf, f)
}
