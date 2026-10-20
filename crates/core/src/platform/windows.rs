//! Windows 预留实现。
//!
//! 计划：隐藏窗口接收 WM_DEVICECHANGE（DBT_DEVICEARRIVAL）监听，
//! CM_Request_Device_Eject 弹出，FILE_FLAG_NO_BUFFERING 替代 F_NOCACHE。

use std::path::Path;

use crossbeam_channel::Sender;

use super::{EjectError, Ejector, VolumeEvent, VolumeWatcher};

pub struct StubWatcher;

impl VolumeWatcher for StubWatcher {
    fn spawn(&self, _tx: Sender<VolumeEvent>) -> anyhow::Result<()> {
        anyhow::bail!("当前平台尚未实现磁盘监听")
    }
}

pub struct StubEjector;

impl Ejector for StubEjector {
    fn eject(&self, _mount: &Path, _bsd: &str) -> Result<(), EjectError> {
        Err(EjectError::Other("当前平台尚未实现弹出".into()))
    }
}

pub struct SleepGuard;

impl SleepGuard {
    pub fn acquire(_reason: &str) -> Option<Self> {
        None
    }
}
