//! 把 watcher → detector → runner 串起来。UI 层只持有 [`Service`]。

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use anyhow::Result;
use crossbeam_channel::{unbounded, Sender};

use crate::config::Config;
use crate::job::{Command, Notice, Runner};
use crate::manifest::{JobRow, Manifest};
use crate::platform::{PlatformEjector, PlatformWatcher, VolumeWatcher};
use crate::progress::{Progress, Snapshot};

pub struct Service {
    pub progress: Arc<Progress>,
    config: Arc<RwLock<Config>>,
    cmd: Sender<Command>,
    manifest_path: PathBuf,
}

impl Service {
    /// 启动监听线程、识别线程和任务线程。通知通过 `notify` 发出。
    pub fn start(config: Config, manifest_path: PathBuf, notify: Sender<Notice>) -> Result<Self> {
        let config = Arc::new(RwLock::new(config));
        let progress = Arc::new(Progress::new());
        let manifest = Manifest::open(&manifest_path)?;
        let (cmd_tx, cmd_rx) = unbounded();

        let runner = Runner::new(config.clone(), progress.clone(), manifest, Box::new(PlatformEjector), notify);
        std::thread::Builder::new().name("job-runner".into()).spawn(move || runner.run(cmd_rx))?;

        let (vol_tx, vol_rx) = unbounded();
        {
            let config = config.clone();
            let cmd_tx = cmd_tx.clone();
            std::thread::Builder::new()
                .name("card-detector".into())
                .spawn(move || crate::detector::run(vol_rx, config, cmd_tx))?;
        }
        PlatformWatcher.spawn(vol_tx)?;

        Ok(Self { progress, config, cmd: cmd_tx, manifest_path })
    }

    pub fn snapshot(&self) -> Snapshot {
        self.progress.snapshot()
    }

    pub fn config(&self) -> Config {
        self.config.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 更新配置；目标变化时尝试恢复挂起的任务
    pub fn set_config(&self, cfg: Config) {
        let target_changed = {
            let mut g = self.config.write().unwrap_or_else(|e| e.into_inner());
            let changed = g.target_path != cfg.target_path || g.target_volume_uuid != cfg.target_volume_uuid;
            *g = cfg;
            changed
        };
        if target_changed {
            let _ = self.cmd.send(Command::TargetMounted);
        }
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.cmd.send(cmd);
    }

    pub fn pause(&self) {
        self.progress.pause();
    }

    pub fn resume(&self) {
        self.progress.resume();
    }

    /// 退出前调用：中止当前拷贝，只留下会被下次清理的 .offload-part
    pub fn shutdown(&self) {
        self.progress.cancel();
        let _ = self.cmd.send(Command::Shutdown);
    }

    pub fn recent_jobs(&self, limit: u32) -> Vec<JobRow> {
        Manifest::open(&self.manifest_path).and_then(|m| m.recent_jobs(limit)).unwrap_or_default()
    }
}
