//! macOS and Linux.

use std::ffi::CString;
use std::fs::{DirEntry, Metadata};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

pub const PATH_SEP: char = '/';

#[cfg(target_os = "macos")]
pub const CASE_INSENSITIVE: bool = true;
#[cfg(not(target_os = "macos"))]
pub const CASE_INSENSITIVE: bool = false;

#[cfg(target_os = "macos")]
pub const ERRORS_HINT: &str = "grant Full Disk Access";
#[cfg(not(target_os = "macos"))]
pub const ERRORS_HINT: &str = "run with sudo";

#[cfg(target_os = "macos")]
pub const OS_NAME: &str = "macOS";
#[cfg(not(target_os = "macos"))]
pub const OS_NAME: &str = "Linux";

pub fn default_root() -> PathBuf {
    PathBuf::from("/")
}

/// Skipped unless `--include-os`: the live operating system and paths that
/// are either unreadable or would be counted twice.
#[cfg(target_os = "macos")]
pub fn os_exclusions() -> &'static [(&'static str, &'static str)] {
    &[
        ("/System", "macos system"),
        ("/dev", "device filesystem"),
        ("/Volumes", "separate volumes - scan them explicitly"),
        ("/private/var/vm", "macos swap"),
        ("/.Spotlight-V100", "system-protected"),
        ("/.fseventsd", "system-protected"),
        ("/.DocumentRevisions-V100", "system-protected"),
        ("/.TemporaryItems", "system-protected"),
        ("/.vol", "system-protected"),
        ("/lost+found", "system-protected"),
    ]
}

#[cfg(not(target_os = "macos"))]
pub fn os_exclusions() -> &'static [(&'static str, &'static str)] {
    &[
        ("/proc", "virtual filesystem"),
        ("/sys", "virtual filesystem"),
        ("/dev", "device filesystem"),
        ("/run", "virtual filesystem"),
        ("/lost+found", "system-protected"),
    ]
}

/// Skipped even with `--include-os`, because descending would double count.
#[cfg(target_os = "macos")]
pub fn always_exclusions() -> &'static [(&'static str, &'static str)] {
    &[("/System/Volumes/Data", "firmlink alias of /")]
}

#[cfg(not(target_os = "macos"))]
pub fn always_exclusions() -> &'static [(&'static str, &'static str)] {
    &[]
}

/// Files at the volume root that the OS sizes for itself.
#[cfg(target_os = "macos")]
pub fn os_managed_names() -> &'static [&'static str] {
    &[]
}

#[cfg(not(target_os = "macos"))]
pub fn os_managed_names() -> &'static [&'static str] {
    &["swapfile", "swap.img"]
}

/// OS-managed files that live somewhere other than the volume root. On
/// macOS the swap files sit in `/private/var/vm`, which is excluded from
/// the walk, so they are listed from here instead.
pub fn collect_os_managed() -> Vec<(String, u64)> {
    let mut found = Vec::new();
    if cfg!(target_os = "macos") {
        if let Ok(entries) = std::fs::read_dir("/private/var/vm") {
            for entry in entries.flatten() {
                if let Ok(md) = entry.metadata() {
                    if md.is_file() {
                        let path =
                            join_path("/private/var/vm", &entry.file_name().to_string_lossy());
                        found.push((path, md.len()));
                    }
                }
            }
        }
    }
    found
}

fn join_path(parent: &str, name: &str) -> String {
    super::join_display(parent, name)
}

pub fn is_volume_root(display: &str) -> bool {
    display == "/"
}

pub fn to_scan_path(root: &Path) -> PathBuf {
    std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf())
}

pub fn to_display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub fn is_link(md: &Metadata) -> bool {
    md.file_type().is_symlink()
}

/// Real allocation from the block count; sparse and compressed files are
/// reported at what they actually occupy. The cluster size is Windows-only.
pub fn allocated_size(md: &Metadata, _cluster: u64) -> u64 {
    md.blocks() * 512
}

pub fn device_of(md: &Metadata) -> u64 {
    md.dev()
}

/// `(device, inode)` of a file with more than one hard link, so the scan can
/// count it once; `None` for a file with a single link. The lstat behind
/// `md` already carries both, so the entry itself is not needed here.
pub fn hardlink_key(_entry: &DirEntry, md: &Metadata) -> Option<(u64, u128)> {
    (md.nlink() > 1).then(|| (md.dev(), u128::from(md.ino())))
}

/// Devices the walk may cross into besides each root's own. On macOS the
/// system volume is firmlinked into the data volume, so a scan of `/` has to
/// step onto the data volume once to see `/Users` at all.
pub fn extra_allowed_devices() -> Vec<u64> {
    let mut devices = Vec::new();
    if cfg!(target_os = "macos") {
        if let Ok(md) = std::fs::metadata("/System/Volumes/Data") {
            devices.push(md.dev());
        }
    }
    devices
}

/// `(total, used, free)` bytes for the filesystem holding `path`, matching
/// what `df` reports: `free` is what an unprivileged user can still write.
#[cfg(target_os = "macos")]
pub fn disk_usage(path: &Path) -> io::Result<(u64, u64, u64)> {
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c_path.as_ptr(), &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let block = st.f_bsize as u64;
    let total = st.f_blocks * block;
    let free = st.f_bavail * block;
    let used = (st.f_blocks - st.f_bfree) * block;
    Ok((total, used, free))
}

#[cfg(not(target_os = "macos"))]
pub fn disk_usage(path: &Path) -> io::Result<(u64, u64, u64)> {
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let block = st.f_frsize as u64;
    let total = st.f_blocks as u64 * block;
    let free = st.f_bavail as u64 * block;
    let used = (st.f_blocks as u64 - st.f_bfree as u64) * block;
    Ok((total, used, free))
}

/// `(dedupe key, label)` for the volume gauge. Two roots on one filesystem
/// share a gauge; the label is the first root scanned on it.
pub fn volume_identity(root: &Path, md: &Metadata) -> (String, String) {
    (
        format!("dev:{}", md.dev()),
        to_display_path(&to_scan_path(root)),
    )
}

pub fn term_width() -> Option<usize> {
    if let Some(cols) = std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    {
        if cols > 0 {
            return Some(cols);
        }
    }
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) };
    if rc == 0 && ws.ws_col > 0 {
        Some(ws.ws_col as usize)
    } else {
        None
    }
}

/// Unix terminals speak VT sequences natively.
pub fn enable_vt() -> bool {
    true
}

extern "C" fn on_sigint(_signal: libc::c_int) {
    crate::scanner::CANCELLED.store(true, Ordering::Relaxed);
}

pub fn install_ctrl_c() {
    unsafe {
        libc::signal(
            libc::SIGINT,
            on_sigint as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
    }
}

/// Local calendar time `(year, month, day, hour, minute, second)` for an
/// epoch timestamp, or `None` when it cannot be represented.
pub fn local_datetime(epoch: f64) -> Option<(i32, u32, u32, u32, u32, u32)> {
    if !epoch.is_finite() {
        return None;
    }
    let secs = epoch.floor();
    if secs < i64::MIN as f64 || secs > i64::MAX as f64 {
        return None;
    }
    let t: libc::time_t = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ptr = unsafe { libc::localtime_r(&t, &mut tm) };
    if ptr.is_null() {
        return None;
    }
    Some((
        tm.tm_year + 1900,
        (tm.tm_mon + 1) as u32,
        tm.tm_mday as u32,
        tm.tm_hour as u32,
        tm.tm_min as u32,
        tm.tm_sec as u32,
    ))
}
