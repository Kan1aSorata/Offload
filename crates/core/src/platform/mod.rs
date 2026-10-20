//! 平台层：所有与操作系统相关的差异都收敛在这里。
//!
//! 其余模块只依赖 [`VolumeWatcher`]、[`Ejector`] 和几个卷信息查询函数，
//! 移植到新平台时只需实现本目录下的一个文件。

use std::path::{Path, PathBuf};

use crossbeam_channel::Sender;
use serde::Serialize;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{MacEjector as PlatformEjector, MacWatcher as PlatformWatcher, SleepGuard};

#[cfg(not(target_os = "macos"))]
mod windows;
#[cfg(not(target_os = "macos"))]
pub use windows::{SleepGuard, StubEjector as PlatformEjector, StubWatcher as PlatformWatcher};

/// 卷挂载事件。回调线程只负责发出它，不做任何文件 I/O。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolumeEvent {
    Mounted {
        bsd: String,
        uuid: Option<String>,
        path: PathBuf,
        name: String,
        size: u64,
    },
    Unmounted {
        bsd: String,
        uuid: Option<String>,
    },
}

/// 监听系统磁盘事件。注册时会对已存在的卷补发一次 `Mounted`。
pub trait VolumeWatcher: Send + Sync {
    fn spawn(&self, tx: Sender<VolumeEvent>) -> anyhow::Result<()>;
}

#[derive(Debug, thiserror::Error)]
pub enum EjectError {
    #[error("卷正被占用{}", .holder.as_deref().map(|h| format!("（{h}）")).unwrap_or_default())]
    Busy { holder: Option<String> },
    #[error("{0}")]
    Other(String),
}

/// 卸载整盘并弹出。
pub trait Ejector: Send + Sync {
    fn eject(&self, mount: &Path, bsd: &str) -> Result<(), EjectError>;
}

/// 某个路径所在卷的信息。
#[derive(Debug, Clone, Serialize)]
pub struct VolumeInfo {
    pub mount_point: PathBuf,
    pub name: String,
    pub uuid: Option<String>,
    pub bsd: Option<String>,
    pub total_bytes: u64,
    pub free_bytes: u64,
    /// 卷根目录下有 DCIM，大概率是相机卡
    pub is_card: bool,
    pub is_root: bool,
}

/// 查询 `path` 所在卷的挂载点、容量和剩余空间（statfs）。
pub fn volume_info(path: &Path) -> Option<VolumeInfo> {
    let st = statfs(path)?;
    let mount_point = PathBuf::from(&st.mount_on);
    let name = if st.mount_on == "/" {
        "Macintosh HD".to_string()
    } else {
        mount_point
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| st.mount_on.clone())
    };
    let bsd = st.mount_from.strip_prefix("/dev/").map(str::to_string);
    let uuid = bsd.as_deref().and_then(volume_uuid_for_bsd);
    Some(VolumeInfo {
        is_card: mount_point.join("DCIM").is_dir(),
        is_root: st.mount_on == "/",
        mount_point,
        name,
        uuid,
        bsd,
        total_bytes: st.total,
        free_bytes: st.avail,
    })
}

/// 同 [`volume_info`]，但路径不存在时使用最近的已存在上级目录（目标目录可能尚未创建）
pub fn volume_info_nearest(path: &Path) -> Option<VolumeInfo> {
    path.ancestors().find(|a| a.exists()).and_then(volume_info)
}

/// 所有已挂载的外置卷（/Volumes 下，排除系统盘）。
pub fn list_external_volumes() -> Vec<VolumeInfo> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir("/Volumes") else {
        return out;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if let Some(info) = volume_info(&p) {
            // /Volumes/Macintosh HD 是指向 / 的链接
            if info.is_root || info.mount_point != p {
                continue;
            }
            // 内置设备上的卷（Recovery 等）不能当目标；内置卡槽里的卡除外
            if !info.is_card && info.bsd.as_deref().is_some_and(is_internal_bsd) {
                continue;
            }
            out.push(info);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 目标路径所在卷当前剩余可用字节数。
pub fn free_space(path: &Path) -> Option<u64> {
    statfs(path).map(|s| s.avail)
}

pub(crate) struct StatFs {
    pub mount_on: String,
    pub mount_from: String,
    pub total: u64,
    pub avail: u64,
}

#[cfg(unix)]
pub(crate) fn statfs(path: &Path) -> Option<StatFs> {
    use std::ffi::{CStr, CString};
    use std::os::unix::ffi::OsStrExt;

    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    #[cfg(target_os = "macos")]
    let (mount_on, mount_from) = unsafe {
        (
            CStr::from_ptr(st.f_mntonname.as_ptr()).to_string_lossy().into_owned(),
            CStr::from_ptr(st.f_mntfromname.as_ptr()).to_string_lossy().into_owned(),
        )
    };
    #[cfg(not(target_os = "macos"))]
    let (mount_on, mount_from) = (path.to_string_lossy().into_owned(), String::new());
    let bsize = st.f_bsize as u64;
    Some(StatFs {
        mount_on,
        mount_from,
        total: st.f_blocks as u64 * bsize,
        avail: st.f_bavail as u64 * bsize,
    })
}

#[cfg(not(unix))]
pub(crate) fn statfs(_path: &Path) -> Option<StatFs> {
    None
}

#[cfg(target_os = "macos")]
pub use macos::{is_internal_bsd, volume_uuid_for_bsd};

#[cfg(not(target_os = "macos"))]
pub fn volume_uuid_for_bsd(_bsd: &str) -> Option<String> {
    None
}

#[cfg(not(target_os = "macos"))]
pub fn is_internal_bsd(_bsd: &str) -> bool {
    false
}

/// 以 F_NOCACHE 方式访问文件：读写绕过统一缓冲区，回读校验才真正读到磁盘。
#[cfg(target_os = "macos")]
pub fn set_nocache(f: &std::fs::File) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    if unsafe { libc::fcntl(f.as_raw_fd(), libc::F_NOCACHE, 1) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn set_nocache(_f: &std::fs::File) -> std::io::Result<()> {
    Ok(())
}
