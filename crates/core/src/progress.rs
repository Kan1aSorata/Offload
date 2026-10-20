//! 进度与状态快照。Rust 侧是唯一可信源，UI 按 10 Hz 采样 [`Progress::snapshot`]。

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// 任务状态机：Detected → Scanning → Copying → Verifying → Ejecting → Done；
/// 任一步出错进入 Failed；目标盘未连接时进入 WaitingForTarget。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    #[default]
    Idle,
    Detected,
    Scanning,
    Copying,
    Verifying,
    Ejecting,
    Done,
    Failed,
    WaitingForTarget,
}

impl JobState {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Detected | Self::Scanning | Self::Copying | Self::Verifying | Self::Ejecting)
    }
}

/// 单个文件内部的阶段。大文件写完后刷盘和回读校验可能要几十秒，
/// 这期间总进度不动，UI 靠它告诉用户「正在校验」而不是看起来卡住了。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStage {
    Copying,
    /// F_FULLFSYNC 刷盘，无法细分进度
    Syncing,
    /// 回读目标盘校验
    Verifying,
    /// 偏执模式：再回读源卡
    VerifyingSource,
}

/// 失败原因，UI 按类型选择文案和操作按钮
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FailReason {
    NoTarget,
    NoSpace { need_bytes: u64, available_bytes: u64, pending_bytes: u64 },
    CardRemoved,
    TargetLost,
    VerifyFailed { count: u32, first: String },
    Io { message: String },
}

impl FailReason {
    pub fn message(&self) -> String {
        match self {
            Self::NoTarget => "尚未设置备份目标".into(),
            Self::NoSpace { need_bytes, .. } => format!("目标盘空间不足（还需 {}）", fmt_bytes(*need_bytes)),
            Self::CardRemoved => "存储卡已被拔出".into(),
            Self::TargetLost => "目标盘已断开".into(),
            Self::VerifyFailed { count, first } => {
                if *count == 1 {
                    format!("{first} 校验未通过")
                } else {
                    format!("{first} 等 {count} 个文件校验未通过")
                }
            }
            Self::Io { message } => format!("读写出错：{message}"),
        }
    }
}

/// 发给前端的状态快照
#[derive(Debug, Clone, Serialize, Default)]
pub struct Snapshot {
    pub state: JobState,
    pub paused: bool,
    pub card_name: Option<String>,
    pub card_capacity: u64,
    pub target_name: Option<String>,
    pub dest_dir: Option<PathBuf>,
    pub current_file: Option<String>,
    pub file_stage: Option<FileStage>,
    /// 当前文件在当前阶段已处理 / 总字节
    pub file_bytes_done: u64,
    pub file_bytes_total: u64,
    pub files_done: u32,
    pub files_total: u32,
    pub files_skipped: u32,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// 最近 3 秒滑动平均
    pub speed_bps: u64,
    /// 整个任务的平均速度（完成后展示）
    pub avg_speed_bps: u64,
    pub eta_secs: Option<u64>,
    pub elapsed_secs: u64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub verified_ok: bool,
    pub ejected: bool,
    pub eject_error: Option<String>,
    pub fail: Option<FailReason>,
    pub last_error: Option<String>,
    pub queued: u32,
}

impl Snapshot {
    /// 按字节计算的进度百分比（0–100）
    pub fn percent(&self) -> u64 {
        (self.bytes_done * 100).checked_div(self.bytes_total).unwrap_or(0).min(100)
    }
}

struct Inner {
    snap: Snapshot,
    started: Option<Instant>,
    finished: Option<Instant>,
    /// (时刻, 已复制字节) 采样，用于 3 秒滑动平均
    samples: VecDeque<(Instant, u64)>,
    /// 暂停累计时长，不计入速度与用时
    paused_total: Duration,
    paused_at: Option<Instant>,
}

pub struct Progress {
    inner: Mutex<Inner>,
    paused: AtomicBool,
    cancel: AtomicBool,
}

#[derive(Debug, thiserror::Error)]
#[error("已取消")]
pub struct Cancelled;

const SPEED_WINDOW: Duration = Duration::from_secs(3);

impl Default for Progress {
    fn default() -> Self {
        Self::new()
    }
}

impl Progress {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                snap: Snapshot::default(),
                started: None,
                finished: None,
                samples: VecDeque::new(),
                paused_total: Duration::ZERO,
                paused_at: None,
            }),
            paused: AtomicBool::new(false),
            cancel: AtomicBool::new(false),
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> R {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut g)
    }

    /// 开始一个新任务，清空上一次的数据
    pub fn begin(&self, card_name: &str, capacity: u64, target_name: Option<String>) {
        self.cancel.store(false, Ordering::SeqCst);
        self.paused.store(false, Ordering::SeqCst);
        self.with(|i| {
            i.snap = Snapshot {
                state: JobState::Detected,
                queued: i.snap.queued,
                card_name: Some(card_name.to_string()),
                card_capacity: capacity,
                target_name,
                started_at: Some(unix_now()),
                ..Default::default()
            };
            i.started = Some(Instant::now());
            i.finished = None;
            i.samples.clear();
            i.paused_total = Duration::ZERO;
            i.paused_at = None;
        });
    }

    pub fn set_state(&self, s: JobState) {
        self.with(|i| {
            i.snap.state = s;
            if matches!(s, JobState::Done | JobState::Failed) {
                i.finished = Some(Instant::now());
                i.snap.finished_at = Some(unix_now());
                i.snap.current_file = None;
                i.snap.file_stage = None;
            }
        });
    }

    pub fn state(&self) -> JobState {
        self.with(|i| i.snap.state)
    }

    pub fn set_plan(&self, files_total: u32, bytes_total: u64, skipped: u32, dest_dir: Option<PathBuf>) {
        self.with(|i| {
            i.snap.files_total = files_total;
            i.snap.bytes_total = bytes_total;
            i.snap.files_skipped = skipped;
            i.snap.dest_dir = dest_dir;
            i.snap.files_done = 0;
            i.snap.bytes_done = 0;
            i.samples.clear();
        });
    }

    pub fn set_current(&self, name: &str, size: u64) {
        self.with(|i| {
            i.snap.current_file = Some(name.to_string());
            i.snap.file_stage = Some(FileStage::Copying);
            i.snap.file_bytes_done = 0;
            i.snap.file_bytes_total = size;
        });
    }

    /// 当前文件进入新阶段，阶段内进度清零
    pub fn set_stage(&self, stage: FileStage) {
        self.with(|i| {
            i.snap.file_stage = Some(stage);
            i.snap.file_bytes_done = 0;
        });
    }

    /// 校验阶段回读了 n 字节，只推进文件内进度，不计入总进度
    pub fn add_stage(&self, n: usize) {
        self.with(|i| i.snap.file_bytes_done += n as u64);
    }

    /// 写入了 n 字节
    pub fn add(&self, n: usize) {
        self.with(|i| {
            i.snap.bytes_done += n as u64;
            i.snap.file_bytes_done += n as u64;
            let now = Instant::now();
            i.samples.push_back((now, i.snap.bytes_done));
            while i.samples.len() > 2 && now.duration_since(i.samples[0].0) > SPEED_WINDOW {
                i.samples.pop_front();
            }
        });
    }

    /// 撤销一个失败文件已计入的字节（重试前调用）
    pub fn sub(&self, n: u64) {
        self.with(|i| i.snap.bytes_done = i.snap.bytes_done.saturating_sub(n));
    }

    pub fn file_done(&self) {
        self.with(|i| i.snap.files_done += 1);
    }

    pub fn finish(&self, verified_ok: bool, ejected: bool, eject_error: Option<String>) {
        self.with(|i| {
            i.snap.verified_ok = verified_ok;
            i.snap.ejected = ejected;
            i.snap.eject_error = eject_error;
        });
    }

    pub fn fail(&self, reason: FailReason) {
        self.with(|i| {
            i.snap.last_error = Some(reason.message());
            i.snap.fail = Some(reason);
        });
        self.set_state(JobState::Failed);
    }

    pub fn set_queued(&self, n: u32) {
        self.with(|i| i.snap.queued = n);
    }

    /// 回到空闲状态（用户点了「完成」）
    pub fn reset(&self) {
        self.with(|i| {
            let queued = i.snap.queued;
            i.snap = Snapshot { queued, ..Default::default() };
            i.started = None;
            i.finished = None;
        });
    }

    pub fn snapshot(&self) -> Snapshot {
        let paused = self.is_paused();
        self.with(|i| {
            let mut s = i.snap.clone();
            s.paused = paused;
            let now = Instant::now();
            let end = i.finished.unwrap_or(now);
            let paused_extra = i.paused_at.map(|p| now.duration_since(p)).unwrap_or_default();
            let elapsed = i
                .started
                .map(|st| end.duration_since(st).saturating_sub(i.paused_total + paused_extra))
                .unwrap_or_default();
            s.elapsed_secs = elapsed.as_secs();
            if elapsed.as_secs_f64() > 0.5 {
                s.avg_speed_bps = (s.bytes_done as f64 / elapsed.as_secs_f64()) as u64;
            }
            if s.state == JobState::Copying && !paused {
                // 3 秒内没有新数据：速度归零
                let fresh = i.samples.back().map(|(t, _)| now.duration_since(*t) < SPEED_WINDOW).unwrap_or(false);
                if fresh && i.samples.len() >= 2 {
                    let (t0, b0) = i.samples[0];
                    let dt = now.duration_since(t0).as_secs_f64().max(0.25);
                    s.speed_bps = ((s.bytes_done - b0) as f64 / dt) as u64;
                }
                let rate = if s.speed_bps > 0 { s.speed_bps } else { s.avg_speed_bps };
                s.eta_secs = s.bytes_total.saturating_sub(s.bytes_done).checked_div(rate);
            }
            s
        })
    }

    // —— 暂停与取消 ——

    pub fn pause(&self) {
        if !self.paused.swap(true, Ordering::SeqCst) {
            self.with(|i| i.paused_at = Some(Instant::now()));
        }
    }

    pub fn resume(&self) {
        if self.paused.swap(false, Ordering::SeqCst) {
            self.with(|i| {
                if let Some(p) = i.paused_at.take() {
                    i.paused_total += p.elapsed();
                }
                i.samples.clear();
            });
        }
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// 拷贝循环在每个分块之间调用：暂停时阻塞，取消时返回错误。
    pub fn checkpoint(&self) -> Result<(), Cancelled> {
        loop {
            if self.is_cancelled() {
                return Err(Cancelled);
            }
            if !self.is_paused() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

pub fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// 十进制单位，与 Finder 一致：48.2 GB
pub fn fmt_bytes(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1000.0 && u < U.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[u])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_format() {
        assert_eq!(fmt_bytes(12_400_000_000), "12.4 GB");
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(2_000_000_000_000), "2.0 TB");
    }

    #[test]
    fn speed_and_eta() {
        let p = Progress::new();
        p.begin("EOS", 0, None);
        p.set_plan(10, 1000, 0, None);
        p.set_state(JobState::Copying);
        p.add(100);
        std::thread::sleep(Duration::from_millis(300));
        p.add(100);
        let s = p.snapshot();
        assert_eq!(s.bytes_done, 200);
        assert!(s.speed_bps > 0);
        assert!(s.eta_secs.is_some());
    }

    #[test]
    fn file_stage_progress() {
        let p = Progress::new();
        p.set_plan(1, 1000, 0, None);
        p.set_current("C0070.MP4", 1000);
        p.add(1000);
        p.set_stage(FileStage::Verifying);
        p.add_stage(400);
        let s = p.snapshot();
        assert_eq!(s.bytes_done, 1000);
        assert_eq!((s.file_stage, s.file_bytes_done, s.file_bytes_total), (Some(FileStage::Verifying), 400, 1000));
        p.set_state(JobState::Done);
        assert_eq!(p.snapshot().file_stage, None);
    }

    #[test]
    fn checkpoint_cancel() {
        let p = Progress::new();
        p.pause();
        p.cancel();
        assert!(p.checkpoint().is_err());
    }
}
