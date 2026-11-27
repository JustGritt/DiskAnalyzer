//! Windows. Paths are walked with the `\\?\` extended-length prefix so deep
//! node_modules trees are not capped at MAX_PATH; reparse points (junctions,
//! symlinks, mount points) are recorded and never followed.

use std::fs::{DirEntry, Metadata};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use windows_sys::Win32::Foundation::{
    CloseHandle, BOOL, FILETIME, HANDLE, INVALID_HANDLE_VALUE, SYSTEMTIME,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FileIdInfo, GetDiskFreeSpaceExW, GetFileInformationByHandle,
    GetFileInformationByHandleEx, BY_HANDLE_FILE_INFORMATION, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_ID_INFO, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};
use windows_sys::Win32::System::Console::{
    GetConsoleMode, GetConsoleScreenBufferInfo, GetStdHandle, SetConsoleCtrlHandler,
    SetConsoleMode, CONSOLE_SCREEN_BUFFER_INFO, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};

use crate::util::round_to_cluster;

pub const PATH_SEP: char = '\\';
pub const CASE_INSENSITIVE: bool = true;
pub const ERRORS_HINT: &str = "run as Administrator";
pub const OS_NAME: &str = "Windows";

const EXTENDED_PREFIX: &str = r"\\?\";
const UNC_EXTENDED_PREFIX: &str = r"\\?\UNC\";
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

pub fn default_root() -> PathBuf {
    PathBuf::from(r"C:\")
}

/// The live operating system, SYSTEM-only paths, and legacy junctions.
pub fn os_exclusions() -> &'static [(&'static str, &'static str)] {
    &[
        (r"C:\Windows", "windows os"),
        (r"C:\Program Files\WindowsApps", "windows os"),
        (r"C:\ProgramData\Microsoft\Windows Defender", "windows os"),
        (
            r"C:\ProgramData\Microsoft\Windows Defender Advanced Threat Protection",
            "windows os",
        ),
        (r"C:\Config.Msi", "windows os"),
        (r"C:\PerfLogs", "windows os"),
        (r"C:\Recovery", "windows os"),
        (r"C:\$SysReset", "windows os"),
        (r"C:\$WinREAgent", "windows os"),
        (r"C:\OneDriveTemp", "windows os"),
        (r"C:\System Volume Information", "system-protected"),
        (r"C:\Documents and Settings", "legacy junction"),
        (r"C:\Users\All Users", "legacy junction"),
        (r"C:\Users\Default User", "legacy junction"),
    ]
}

pub fn always_exclusions() -> &'static [(&'static str, &'static str)] {
    &[]
}

pub fn os_managed_names() -> &'static [&'static str] {
    &[
        "pagefile.sys",
        "hiberfil.sys",
        "swapfile.sys",
        "dumpstack.log",
        "dumpstack.log.tmp",
        "bootmgr",
        "bootnxt",
    ]
}

pub fn collect_os_managed() -> Vec<(String, u64)> {
    Vec::new()
}

pub fn is_volume_root(display: &str) -> bool {
    let stripped = display.trim_end_matches(['\\', '/']);
    stripped.len() == 2 && stripped.ends_with(':')
}

/// `\\?\`-prefixed absolute path so the walk is not capped at MAX_PATH.
pub fn to_scan_path(root: &Path) -> PathBuf {
    let text = root.to_string_lossy();
    if text.starts_with(EXTENDED_PREFIX) {
        return root.to_path_buf();
    }
    let absolute = std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf());
    let abs_text = absolute.to_string_lossy();
    if let Some(rest) = abs_text.strip_prefix(r"\\") {
        return PathBuf::from(format!("{UNC_EXTENDED_PREFIX}{rest}"));
    }
    PathBuf::from(format!("{EXTENDED_PREFIX}{abs_text}"))
}

pub fn to_display_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(UNC_EXTENDED_PREFIX) {
        return format!(r"\\{rest}");
    }
    if let Some(rest) = text.strip_prefix(EXTENDED_PREFIX) {
        return rest.to_string();
    }
    text.into_owned()
}

pub fn is_link(md: &Metadata) -> bool {
    md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

pub fn allocated_size(md: &Metadata, cluster: u64) -> u64 {
    round_to_cluster(md.len(), cluster)
}

pub fn device_of(_md: &Metadata) -> u64 {
    0
}

/// `(volume serial, file ID)` of a file with more than one hard link, so the
/// scan can count it once; `None` for a file with a single link, or one whose
/// attributes cannot be read (it is then counted, as without the option).
///
/// A directory listing's `Metadata` carries neither the link count nor the
/// file ID on Windows, so the file is opened, read-only in the narrowest way
/// the API allows: FILE_READ_ATTRIBUTES access and no data access (no
/// antivirus scan on open, no last-access update), every share mode so no
/// other program is blocked, OPEN_EXISTING so nothing can be created, and
/// FILE_FLAG_OPEN_REPARSE_POINT so nothing is followed.
pub fn hardlink_key(entry: &DirEntry, _md: &Metadata) -> Option<(u64, u128)> {
    hardlink_key_at(&entry.path())
}

/// [`hardlink_key`] for a display path instead of a directory entry.
pub fn hardlink_key_of(display: &str) -> Option<(u64, u128)> {
    // CreateFileW caps a path at MAX_PATH unless it carries the prefix.
    hardlink_key_at(&to_scan_path(Path::new(display)))
}

fn hardlink_key_at(path: &Path) -> Option<(u64, u128)> {
    let handle = AttributeHandle::open(path)?;
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(handle.0, &mut info) } == 0 || info.nNumberOfLinks <= 1 {
        return None;
    }
    // Only NTFS guarantees the 64-bit index above is unique; ReFS (Dev
    // Drives) needs the 128-bit ID. Asked for linked files only.
    let mut id: FILE_ID_INFO = unsafe { std::mem::zeroed() };
    let has_id = unsafe {
        GetFileInformationByHandleEx(
            handle.0,
            FileIdInfo,
            (&mut id as *mut FILE_ID_INFO).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    } != 0;
    Some(if has_id {
        (
            id.VolumeSerialNumber,
            u128::from_le_bytes(id.FileId.Identifier),
        )
    } else {
        (
            u64::from(info.dwVolumeSerialNumber),
            (u128::from(info.nFileIndexHigh) << 32) | u128::from(info.nFileIndexLow),
        )
    })
}

/// A handle opened for reading attributes only, closed on drop.
struct AttributeHandle(HANDLE);

impl AttributeHandle {
    fn open(path: &Path) -> Option<Self> {
        let wide_path = wide(path);
        let handle = unsafe {
            CreateFileW(
                wide_path.as_ptr(),
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        (handle != INVALID_HANDLE_VALUE).then_some(Self(handle))
    }
}

impl Drop for AttributeHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

pub fn extra_allowed_devices() -> Vec<u64> {
    Vec::new()
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

pub fn disk_usage(path: &Path) -> io::Result<(u64, u64, u64)> {
    let wide_path = wide(path);
    let mut free_to_caller: u64 = 0;
    let mut total: u64 = 0;
    let mut free_total: u64 = 0;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide_path.as_ptr(),
            &mut free_to_caller,
            &mut total,
            &mut free_total,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((total, total.saturating_sub(free_total), free_total))
}

/// Keyed by drive so two roots on one drive share a gauge labelled `C:\`.
pub fn volume_identity(root: &Path, _md: &Metadata) -> (String, String) {
    let display = to_display_path(&to_scan_path(root));
    if let Some(rest) = display.strip_prefix(r"\\") {
        // UNC: \\server\share
        let mut parts = rest.splitn(3, '\\');
        let server = parts.next().unwrap_or("");
        let share = parts.next().unwrap_or("");
        let label = format!(r"\\{server}\{share}");
        return (label.to_lowercase(), label);
    }
    let bytes = display.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' {
        let label = format!("{}:\\", display[..1].to_uppercase());
        return (label.to_lowercase(), label);
    }
    (display.to_lowercase(), display)
}

pub fn term_width() -> Option<usize> {
    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
        if GetConsoleScreenBufferInfo(handle, &mut info) == 0 {
            return None;
        }
        let width = info.srWindow.Right as i32 - info.srWindow.Left as i32 + 1;
        (width > 0).then_some(width as usize)
    }
}

/// Ask the console to interpret VT sequences; false means stay monochrome.
pub fn enable_vt() -> bool {
    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut mode: u32 = 0;
        if GetConsoleMode(handle, &mut mode) == 0 {
            return false;
        }
        if mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0 {
            return true;
        }
        SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0
    }
}

unsafe extern "system" fn on_ctrl(_ctrl_type: u32) -> BOOL {
    crate::scanner::CANCELLED.store(true, Ordering::Relaxed);
    1
}

pub fn install_ctrl_c() {
    unsafe {
        SetConsoleCtrlHandler(Some(on_ctrl), 1);
    }
}

pub fn local_datetime(epoch: f64) -> Option<(i32, u32, u32, u32, u32, u32)> {
    if !epoch.is_finite() || epoch < -11_644_473_600.0 {
        return None;
    }
    let ticks = (epoch * 10_000_000.0) as i128 + 116_444_736_000_000_000i128;
    if ticks < 0 || ticks > u64::MAX as i128 {
        return None;
    }
    let ticks = ticks as u64;
    let ft = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    unsafe {
        let mut utc: SYSTEMTIME = std::mem::zeroed();
        if FileTimeToSystemTime(&ft, &mut utc) == 0 {
            return None;
        }
        let mut local: SYSTEMTIME = std::mem::zeroed();
        if SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) == 0 {
            return None;
        }
        Some((
            local.wYear as i32,
            local.wMonth as u32,
            local.wDay as u32,
            local.wHour as u32,
            local.wMinute as u32,
            local.wSecond as u32,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extended_prefix_round_trips() {
        let scanned = to_scan_path(Path::new(r"C:\Users"));
        assert_eq!(scanned.to_string_lossy(), r"\\?\C:\Users");
        assert_eq!(to_display_path(&scanned), r"C:\Users");
        // Already prefixed paths are left alone.
        let already = Path::new(r"\\?\C:\Users");
        assert_eq!(to_scan_path(already), already);
        // UNC paths take the UNC form of the prefix.
        let unc = to_scan_path(Path::new(r"\\server\share\dir"));
        assert_eq!(unc.to_string_lossy(), r"\\?\UNC\server\share\dir");
        assert_eq!(to_display_path(&unc), r"\\server\share\dir");
        // A path with no prefix passes through display unchanged.
        assert_eq!(to_display_path(Path::new(r"D:\data")), r"D:\data");
    }

    #[test]
    fn volume_roots_are_drive_letters_only() {
        assert!(is_volume_root(r"C:\"));
        assert!(is_volume_root("C:"));
        assert!(!is_volume_root(r"C:\Users"));
        assert!(!is_volume_root(r"\\server\share"));
    }

    #[test]
    fn volumes_are_keyed_by_drive() {
        let md = std::fs::metadata(".").unwrap();
        let (key_a, label) = volume_identity(Path::new(r"C:\Users"), &md);
        let (key_b, _) = volume_identity(Path::new(r"c:\Windows\Temp"), &md);
        // Two roots on one drive share a gauge, labelled by the drive.
        assert_eq!(key_a, key_b);
        assert_eq!(label, r"C:\");
    }

    #[test]
    fn allocated_size_rounds_up_to_clusters() {
        // Exercised through the pure helper the Windows path uses.
        assert_eq!(round_to_cluster(1, 4096), 4096);
        assert_eq!(round_to_cluster(4097, 4096), 8192);
    }
}
