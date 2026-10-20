//! macOS 平台层：DiskArbitration 监听与弹出，IOKit 电源断言。

use std::ffi::{c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_foundation::url::CFURL;
use core_foundation_sys::array::CFArrayRef;
use core_foundation_sys::base::{kCFAllocatorDefault, CFAllocatorRef, CFRelease, CFTypeRef};
use core_foundation_sys::dictionary::{CFDictionaryGetValue, CFDictionaryRef};
use core_foundation_sys::runloop::{kCFRunLoopDefaultMode, CFRunLoopGetCurrent, CFRunLoopRef, CFRunLoopRun};
use core_foundation_sys::string::CFStringRef;
use core_foundation_sys::url::CFURLRef;
use core_foundation_sys::uuid::{CFUUIDCreateString, CFUUIDRef};
use crossbeam_channel::Sender;

use super::{EjectError, Ejector, VolumeEvent, VolumeWatcher};

type DASessionRef = *const c_void;
type DADiskRef = *const c_void;
type DiskCb = extern "C" fn(DADiskRef, *mut c_void);
type DiskChangedCb = extern "C" fn(DADiskRef, CFArrayRef, *mut c_void);

#[link(name = "DiskArbitration", kind = "framework")]
extern "C" {
    fn DASessionCreate(a: CFAllocatorRef) -> DASessionRef;
    fn DARegisterDiskAppearedCallback(s: DASessionRef, m: CFDictionaryRef, cb: DiskCb, ctx: *mut c_void);
    fn DARegisterDiskDescriptionChangedCallback(
        s: DASessionRef,
        m: CFDictionaryRef,
        watch: CFArrayRef,
        cb: DiskChangedCb,
        ctx: *mut c_void,
    );
    fn DARegisterDiskDisappearedCallback(s: DASessionRef, m: CFDictionaryRef, cb: DiskCb, ctx: *mut c_void);
    fn DASessionScheduleWithRunLoop(s: DASessionRef, rl: CFRunLoopRef, mode: CFStringRef);
    fn DADiskCopyDescription(d: DADiskRef) -> CFDictionaryRef;
    fn DADiskCreateFromBSDName(a: CFAllocatorRef, s: DASessionRef, name: *const libc::c_char) -> DADiskRef;

    static kDADiskDescriptionVolumeMountableKey: CFStringRef;
    static kDADiskDescriptionVolumePathKey: CFStringRef;
    static kDADiskDescriptionVolumeUUIDKey: CFStringRef;
    static kDADiskDescriptionVolumeNameKey: CFStringRef;
    static kDADiskDescriptionMediaBSDNameKey: CFStringRef;
    static kDADiskDescriptionMediaSizeKey: CFStringRef;
    static kDADiskDescriptionDeviceInternalKey: CFStringRef;
    static kDADiskDescriptionWatchVolumePath: CFArrayRef;
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOPMAssertionCreateWithName(t: CFStringRef, level: u32, name: CFStringRef, id: *mut u32) -> i32;
    fn IOPMAssertionRelease(id: u32) -> i32;
}

/// 从 DiskArbitration 描述字典里取出的字段
struct DiskDesc {
    bsd: String,
    uuid: Option<String>,
    path: Option<PathBuf>,
    name: String,
    size: u64,
}

unsafe fn dict_get(d: CFDictionaryRef, key: CFStringRef) -> Option<CFTypeRef> {
    let v = CFDictionaryGetValue(d, key as *const c_void);
    (!v.is_null()).then_some(v)
}

unsafe fn describe(disk: DADiskRef) -> Option<DiskDesc> {
    let d = DADiskCopyDescription(disk);
    if d.is_null() {
        return None;
    }
    let get_string = |k: CFStringRef| {
        dict_get(d, k).map(|v| CFString::wrap_under_get_rule(v as CFStringRef).to_string())
    };
    let bsd = get_string(kDADiskDescriptionMediaBSDNameKey).unwrap_or_default();
    let name = get_string(kDADiskDescriptionVolumeNameKey).unwrap_or_default();
    let path = dict_get(d, kDADiskDescriptionVolumePathKey)
        .and_then(|v| CFURL::wrap_under_get_rule(v as CFURLRef).to_path());
    let uuid = dict_get(d, kDADiskDescriptionVolumeUUIDKey).map(|v| {
        let s = CFUUIDCreateString(kCFAllocatorDefault, v as CFUUIDRef);
        CFString::wrap_under_create_rule(s).to_string()
    });
    let size = dict_get(d, kDADiskDescriptionMediaSizeKey)
        .and_then(|v| CFNumber::wrap_under_get_rule(v as _).to_i64())
        .unwrap_or(0) as u64;
    CFRelease(d as CFTypeRef);
    Some(DiskDesc { bsd, uuid, path, name, size })
}

fn send_for(disk: DADiskRef, ctx: *mut c_void, gone: bool) {
    // 规则：回调里不做任何文件 I/O，否则会阻塞系统的磁盘仲裁
    let tx = unsafe { &*(ctx as *const Sender<VolumeEvent>) };
    let Some(d) = (unsafe { describe(disk) }) else { return };
    let ev = match (gone, d.path) {
        (false, Some(path)) => VolumeEvent::Mounted { bsd: d.bsd, uuid: d.uuid, path, name: d.name, size: d.size },
        _ => VolumeEvent::Unmounted { bsd: d.bsd, uuid: d.uuid },
    };
    let _ = tx.send(ev);
}

extern "C" fn on_disk(disk: DADiskRef, ctx: *mut c_void) {
    // DiskAppeared 可能早于挂载触发，此时没有 VolumePath，等 DescriptionChanged 再发
    let Some(d) = (unsafe { describe(disk) }) else { return };
    if d.path.is_some() {
        send_for(disk, ctx, false);
    }
}

extern "C" fn on_changed(disk: DADiskRef, _keys: CFArrayRef, ctx: *mut c_void) {
    // VolumePath 从无到有 = 挂载完成；从有到无 = 已卸载
    send_for(disk, ctx, false);
}

extern "C" fn on_gone(disk: DADiskRef, ctx: *mut c_void) {
    send_for(disk, ctx, true);
}

/// 在独立线程上运行 CFRunLoop 的 DiskArbitration 监听器
pub struct MacWatcher;

impl VolumeWatcher for MacWatcher {
    fn spawn(&self, tx: Sender<VolumeEvent>) -> anyhow::Result<()> {
        std::thread::Builder::new().name("disk-arb".into()).spawn(move || unsafe {
            let s = DASessionCreate(kCFAllocatorDefault);
            let ctx = Box::into_raw(Box::new(tx)) as *mut c_void; // 生命周期 = 进程
            // 只匹配可挂载的卷，过滤掉整盘和分区表
            let m = CFDictionary::<CFString, CFType>::from_CFType_pairs(&[(
                CFString::wrap_under_get_rule(kDADiskDescriptionVolumeMountableKey),
                CFBoolean::true_value().as_CFType(),
            )]);
            let mref = m.as_concrete_TypeRef();
            DARegisterDiskAppearedCallback(s, mref, on_disk, ctx);
            DARegisterDiskDescriptionChangedCallback(s, mref, kDADiskDescriptionWatchVolumePath, on_changed, ctx);
            DARegisterDiskDisappearedCallback(s, mref, on_gone, ctx);
            DASessionScheduleWithRunLoop(s, CFRunLoopGetCurrent(), kCFRunLoopDefaultMode);
            std::mem::forget(m);
            CFRunLoopRun(); // 线程常驻
        })?;
        Ok(())
    }
}

/// 用 BSD 名打开 DADisk 并对其描述字典执行 `f`
fn with_disk<R>(bsd: &str, f: impl FnOnce(DADiskRef) -> Option<R>) -> Option<R> {
    let c = CString::new(bsd).ok()?;
    unsafe {
        let s = DASessionCreate(kCFAllocatorDefault);
        if s.is_null() {
            return None;
        }
        let disk = DADiskCreateFromBSDName(kCFAllocatorDefault, s, c.as_ptr());
        let r = if disk.is_null() {
            None
        } else {
            let r = f(disk);
            CFRelease(disk as CFTypeRef);
            r
        };
        CFRelease(s as CFTypeRef);
        r
    }
}

/// 通过 BSD 名（如 disk4s1）查询卷 UUID，用于识别目标盘。
pub fn volume_uuid_for_bsd(bsd: &str) -> Option<String> {
    with_disk(bsd, |disk| unsafe { describe(disk) }.and_then(|d| d.uuid))
}

/// 卷是否位于内置设备上（系统盘、Recovery 等）
pub fn is_internal_bsd(bsd: &str) -> bool {
    with_disk(bsd, |disk| unsafe {
        let d = DADiskCopyDescription(disk);
        if d.is_null() {
            return None;
        }
        let v = dict_get(d, kDADiskDescriptionDeviceInternalKey)
            .map(|v| CFBoolean::wrap_under_get_rule(v as _) == CFBoolean::true_value());
        CFRelease(d as CFTypeRef);
        v
    })
    .unwrap_or(false)
}

/// MVP 用 `diskutil eject` 卸载整盘并弹出，失败时按 1、2、4 秒退避重试。
pub struct MacEjector;

impl Ejector for MacEjector {
    fn eject(&self, mount: &Path, bsd: &str) -> Result<(), EjectError> {
        let target = if bsd.is_empty() {
            mount.to_string_lossy().into_owned()
        } else {
            format!("/dev/{bsd}")
        };
        let mut last = EjectError::Other("未知错误".into());
        for (i, wait) in [0u64, 1, 2, 4].into_iter().enumerate() {
            if i > 0 {
                std::thread::sleep(Duration::from_secs(wait));
            }
            match Command::new("/usr/sbin/diskutil").arg("eject").arg(&target).output() {
                Ok(o) if o.status.success() => return Ok(()),
                Ok(o) => {
                    let msg = format!(
                        "{}{}",
                        String::from_utf8_lossy(&o.stdout),
                        String::from_utf8_lossy(&o.stderr)
                    );
                    // 卷已经不在了：视为弹出成功
                    if !mount.exists() {
                        return Ok(());
                    }
                    last = parse_dissenter(&msg)
                        .map(|holder| EjectError::Busy { holder: Some(holder) })
                        .unwrap_or_else(|| {
                            if msg.contains("busy") || msg.contains("dissented") {
                                EjectError::Busy { holder: None }
                            } else {
                                EjectError::Other(msg.trim().to_string())
                            }
                        });
                    tracing::warn!(attempt = i, %msg, "eject failed");
                }
                Err(e) => last = EjectError::Other(e.to_string()),
            }
        }
        Err(last)
    }
}

/// 从 diskutil 输出中解析占用进程，如 "dissented by PID 123 (/usr/bin/foo)"
fn parse_dissenter(msg: &str) -> Option<String> {
    let idx = msg.find("PID ")?;
    let rest = &msg[idx + 4..];
    let pid: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if pid.is_empty() {
        return None;
    }
    let name = rest
        .find('(')
        .and_then(|a| rest[a + 1..].find(')').map(|b| &rest[a + 1..a + 1 + b]))
        .map(|p| p.rsplit('/').next().unwrap_or(p).to_string())
        .or_else(|| process_name(&pid));
    Some(match name {
        Some(n) => format!("{n}, PID {pid}"),
        None => format!("PID {pid}"),
    })
}

fn process_name(pid: &str) -> Option<String> {
    let o = Command::new("/bin/ps").args(["-p", pid, "-o", "comm="]).output().ok()?;
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (!s.is_empty()).then(|| s.rsplit('/').next().unwrap_or(&s).to_string())
}

/// 备份期间阻止系统空闲睡眠（IOPMAssertion），drop 时释放。
pub struct SleepGuard(u32);

impl SleepGuard {
    pub fn acquire(reason: &str) -> Option<Self> {
        let t = CFString::from_static_string("PreventUserIdleSystemSleep");
        let n = CFString::new(reason);
        let mut id = 0u32;
        let rc = unsafe {
            IOPMAssertionCreateWithName(t.as_concrete_TypeRef(), 255, n.as_concrete_TypeRef(), &mut id)
        };
        (rc == 0).then_some(SleepGuard(id))
    }
}

impl Drop for SleepGuard {
    fn drop(&mut self) {
        unsafe { IOPMAssertionRelease(self.0) };
    }
}

#[allow(dead_code)]
fn cstr(p: *const libc::c_char) -> String {
    unsafe { CStr::from_ptr(p).to_string_lossy().into_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dissenter_with_path() {
        let m = "Unmount of disk4 failed: at least one volume could not be unmounted\nUnmount failed for /Volumes/EOS: dissented by PID 412 (/System/Library/Frameworks/CoreServices.framework/mds)";
        assert_eq!(parse_dissenter(m).as_deref(), Some("mds, PID 412"));
    }
}
