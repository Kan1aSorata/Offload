//! 任务队列与执行。同一目标盘一次只跑一个任务；目标盘未连接时挂起。
//!
//! 所有异常路径的共同底线：失败时不弹卡、不留下看似完整的文件、下次插卡可自动续传。

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};

use crate::config::Config;
use crate::copier::{copy_verified, CopyError, CopyOptions};
use crate::manifest::{FileRecord, Manifest};
use crate::planner::{self, PlanInput};
use crate::platform::{Ejector, SleepGuard};
use crate::progress::{fmt_bytes, FailReason, JobState, Progress};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// 卷 UUID，或兜底标识
    pub card_id: String,
    pub bsd: String,
    pub mount: PathBuf,
    pub name: String,
    pub capacity: u64,
}

#[derive(Debug)]
pub enum Command {
    Enqueue(Job),
    /// 目标盘挂载或目标设置变化：恢复挂起的任务
    TargetMounted,
    CardRemoved { bsd: String },
    /// 重试上一次失败的任务
    Retry,
    /// 用户关闭结果页，回到空闲
    Dismiss,
    Shutdown,
}

/// 发给 UI 层的系统通知
#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    Started { card: String, files: u32, bytes: u64 },
    Done { card: String, files: u32, ejected: bool, eject_error: Option<String>, auto_eject: bool },
    NoNewFiles { card: String, ejected: bool },
    Failed { card: String, reason: FailReason },
    WaitingForTarget { card: String, target: String },
}

impl Notice {
    pub fn title(&self) -> String {
        match self {
            Self::Started { card, .. } => format!("正在备份「{card}」"),
            Self::Done { .. } => "备份完成".into(),
            Self::NoNewFiles { .. } => "没有新文件".into(),
            Self::Failed { reason: FailReason::VerifyFailed { count, .. }, .. } => format!("{count} 个文件校验未通过"),
            Self::Failed { .. } => "备份已停止".into(),
            Self::WaitingForTarget { .. } => "等待目标盘".into(),
        }
    }

    pub fn body(&self) -> String {
        match self {
            Self::Started { files, bytes, .. } => format!("{files} 个新文件，{}", fmt_bytes(*bytes)),
            Self::Done { files, ejected: true, .. } => format!("已备份 {files} 个文件并全部校验，卡已弹出"),
            Self::Done { files, eject_error: Some(e), .. } => {
                format!("已备份 {files} 个文件并全部校验，但卡未能弹出：{e}")
            }
            Self::Done { files, auto_eject: false, .. } => format!("已备份 {files} 个文件并全部校验，可以拔出卡"),
            Self::Done { files, .. } => format!("已备份 {files} 个文件并全部校验"),
            Self::NoNewFiles { card, ejected: true } => format!("「{card}」没有新文件，卡已弹出"),
            Self::NoNewFiles { card, .. } => format!("「{card}」没有新文件"),
            Self::Failed { reason: FailReason::VerifyFailed { count, first }, .. } => {
                let who = if *count == 1 { first.clone() } else { format!("{first} 等 {count} 个文件") };
                format!("{who}需重新复制，原文件仍在卡上，卡未弹出")
            }
            Self::Failed { reason, .. } => format!("备份中断：{}，卡未弹出", reason.message()),
            Self::WaitingForTarget { target, .. } => format!("未找到 {target}，接入后将自动继续，卡未弹出"),
        }
    }
}

pub struct Runner {
    cfg: Arc<RwLock<Config>>,
    progress: Arc<Progress>,
    manifest: Manifest,
    ejector: Box<dyn Ejector>,
    notify: Sender<Notice>,
    queue: VecDeque<Job>,
    /// 等待目标盘的卡，按挂起顺序；弹窗显示最后一张
    waiting: Vec<Job>,
    /// 失败的卡，「重试」时全部重新排队
    failed: Vec<Job>,
    /// 本次运行中已完成的卡及完成时刻：睡眠唤醒、扩展坞重连导致的重复挂载不重复提示
    completed: HashMap<String, Instant>,
}

/// 完成后这段时间内同一张卡再次挂载，视为系统重新挂载
const REMOUNT_WINDOW: Duration = Duration::from_secs(60);

enum Outcome {
    Finished,
    Waiting,
    Failed,
    Silent,
}

impl Runner {
    pub fn new(
        cfg: Arc<RwLock<Config>>,
        progress: Arc<Progress>,
        manifest: Manifest,
        ejector: Box<dyn Ejector>,
        notify: Sender<Notice>,
    ) -> Self {
        let _ = manifest.close_dangling_jobs();
        Self {
            cfg,
            progress,
            manifest,
            ejector,
            notify,
            queue: VecDeque::new(),
            waiting: Vec::new(),
            failed: Vec::new(),
            completed: HashMap::new(),
        }
    }

    fn config(&self) -> Config {
        self.cfg.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn is_known(&self, card_id: &str) -> bool {
        self.queue.iter().any(|j| j.card_id == card_id) || self.waiting.iter().any(|j| j.card_id == card_id)
    }

    /// 把一组卡按原顺序排到队首
    fn requeue_front(&mut self, jobs: Vec<Job>) {
        for job in jobs.into_iter().rev() {
            self.queue.push_front(job);
        }
    }

    /// 等待中的卡变动后，弹窗改为显示最后挂起的那张；没有了就回到空闲
    fn show_waiting(&self) {
        let Some(job) = self.waiting.last() else {
            self.progress.reset();
            return;
        };
        let cfg = self.config();
        self.progress.begin(&job.name, job.capacity, cfg.target_name());
        if cfg.target_path.is_none() {
            self.progress.fail(FailReason::NoTarget);
        } else {
            self.progress.set_state(JobState::WaitingForTarget);
        }
    }

    /// 取出队首的卡执行一次；队列为空时返回 false
    fn run_next(&mut self) -> bool {
        let Some(job) = self.queue.pop_front() else {
            return false;
        };
        self.progress.set_queued(self.queue.len() as u32);
        match self.run_job(&job) {
            Outcome::Waiting => self.waiting.push(job),
            Outcome::Failed => self.failed.push(job),
            Outcome::Finished | Outcome::Silent => {}
        }
        true
    }

    /// 事件循环：在专用线程上运行，直到收到 Shutdown
    pub fn run(mut self, rx: Receiver<Command>) {
        loop {
            while let Ok(cmd) = rx.try_recv() {
                if !self.handle(cmd) {
                    return;
                }
            }
            self.progress.set_queued(self.queue.len() as u32);
            if self.run_next() {
                continue;
            }
            match rx.recv() {
                Ok(cmd) => {
                    if !self.handle(cmd) {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    }

    fn handle(&mut self, cmd: Command) -> bool {
        match cmd {
            Command::Enqueue(job) => {
                if !self.is_known(&job.card_id) {
                    tracing::info!(card = %job.name, id = %job.card_id, "job queued");
                    self.failed.retain(|j| j.card_id != job.card_id);
                    self.queue.push_back(job);
                }
            }
            Command::TargetMounted => {
                if !self.waiting.is_empty() && self.config().target_connected() {
                    let jobs = std::mem::take(&mut self.waiting);
                    self.requeue_front(jobs);
                }
            }
            Command::CardRemoved { bsd } => {
                if bsd.is_empty() {
                    return true;
                }
                self.queue.retain(|j| j.bsd != bsd);
                let before = self.waiting.len();
                self.waiting.retain(|j| j.bsd != bsd);
                if self.waiting.len() != before {
                    self.show_waiting();
                }
                self.failed.retain(|j| j.bsd != bsd);
            }
            Command::Retry => {
                // 重试全部失败的卡；没有失败的卡时，重试等待目标盘的卡
                let jobs = if self.failed.is_empty() {
                    std::mem::take(&mut self.waiting)
                } else {
                    std::mem::take(&mut self.failed)
                };
                self.requeue_front(jobs);
            }
            Command::Dismiss => {
                // 放弃等待目标盘的任务（卡保持未弹出）
                if self.progress.state() == JobState::WaitingForTarget {
                    self.waiting.clear();
                    self.progress.reset();
                }
                if matches!(self.progress.state(), JobState::Done | JobState::Failed) {
                    self.failed.clear();
                    self.waiting.clear();
                    self.progress.reset();
                }
            }
            Command::Shutdown => return false,
        }
        true
    }

    fn fail(&self, job: &Job, job_id: Option<i64>, reason: FailReason) -> Outcome {
        tracing::warn!(card = %job.name, ?reason, "job failed");
        if let Some(id) = job_id {
            let s = self.progress.snapshot();
            let _ = self.manifest.job_finish(id, "failed", s.files_done, s.files_skipped, s.bytes_done, Some(&reason.message()), None);
        }
        self.progress.fail(reason.clone());
        let _ = self.notify.send(Notice::Failed { card: job.name.clone(), reason });
        Outcome::Failed
    }

    fn wait_for_target(&self, job: &Job, cfg: &Config) -> Outcome {
        self.progress.set_state(JobState::WaitingForTarget);
        let target = cfg.target_name().unwrap_or_else(|| "目标盘".into());
        let _ = self.notify.send(Notice::WaitingForTarget { card: job.name.clone(), target });
        Outcome::Waiting
    }

    fn run_job(&mut self, job: &Job) -> Outcome {
        let cfg = self.config();
        self.progress.begin(&job.name, job.capacity, cfg.target_name());
        if !crate::detector::is_card(&job.mount) {
            self.progress.reset();
            return Outcome::Silent;
        }
        let Some(target) = cfg.target_path.clone() else {
            // 挂起：用户选好目标后（TargetMounted）自动开始
            self.fail(job, None, FailReason::NoTarget);
            return Outcome::Waiting;
        };
        if !cfg.target_connected() {
            return self.wait_for_target(job, &cfg);
        }

        self.progress.set_state(JobState::Scanning);
        if let Err(e) = std::fs::create_dir_all(&target) {
            return self.fail(job, None, FailReason::Io { message: e.to_string() });
        }
        let layout = cfg.folder_template();
        let plan = match planner::plan(&PlanInput {
            card_root: &job.mount,
            card_id: &job.card_id,
            card_name: &job.name,
            target_root: &target,
            layout: &layout,
            manifest: &self.manifest,
            include_proxies: cfg.backup_proxies,
        }) {
            Ok(p) => p,
            Err(e) => {
                let reason = if crate::detector::is_card(&job.mount) {
                    FailReason::Io { message: e.to_string() }
                } else {
                    FailReason::CardRemoved
                };
                return self.fail(job, None, reason);
            }
        };
        // 规则 2：目标已存在的文件补记 manifest（哈希留空）
        for s in plan.skipped.iter().filter(|s| s.needs_record) {
            let _ = self.manifest.record(&FileRecord {
                volume_uuid: &job.card_id,
                rel_path: &s.rel_path,
                size: s.size,
                mtime: s.mtime,
                hash: None,
                dest_path: &s.dst,
            });
        }

        let job_id = self.manifest.job_start(&job.card_id, &job.name).ok();
        let display_dir = plan.display_dir(&target);
        let files_total = plan.tasks.len() as u32;
        self.progress.set_plan(files_total, plan.bytes_total, plan.skipped.len() as u32, Some(display_dir.clone()));
        tracing::info!(card = %job.name, files = files_total, bytes = plan.bytes_total, skipped = plan.skipped.len(), "plan ready");

        if plan.tasks.is_empty() {
            if let Some(id) = job_id {
                let _ = self.manifest.job_finish(id, "no_new_files", 0, plan.skipped.len() as u32, 0, None, Some(&display_dir));
            }
            // 睡眠唤醒或扩展坞重连导致的重复挂载：静默。
            // 卡开启自动弹出时只有刚完成不久才可能被重新挂载；用户隔一段时间手动重插则正常提示并弹出。
            let remount = self.completed.get(&job.card_id).is_some_and(|t| !cfg.auto_eject || t.elapsed() < REMOUNT_WINDOW);
            if remount {
                self.progress.reset();
                return Outcome::Silent;
            }
            let (ejected, eject_error) = self.maybe_eject(job, &cfg);
            self.progress.finish(true, ejected, eject_error);
            self.progress.set_state(JobState::Done);
            self.completed.insert(job.card_id.clone(), Instant::now());
            let _ = self.notify.send(Notice::NoNewFiles { card: job.name.clone(), ejected });
            return Outcome::Finished;
        }

        if let Err(s) = planner::check_space(plan.bytes_total, &target) {
            return self.fail(
                job,
                job_id,
                FailReason::NoSpace { need_bytes: s.need, available_bytes: s.available, pending_bytes: s.pending },
            );
        }
        planner::clean_stale_parts(&plan);

        let _ = self.notify.send(Notice::Started { card: job.name.clone(), files: files_total, bytes: plan.bytes_total });
        self.progress.set_state(JobState::Copying);
        let sleep_guard = SleepGuard::acquire("Offload 正在备份存储卡");
        let opts = CopyOptions { verify: cfg.verify, paranoid: cfg.paranoid_verify };

        let mut copied: Vec<&planner::CopyTask> = Vec::with_capacity(plan.tasks.len());
        let mut bad: Vec<String> = Vec::new();
        for task in &plan.tasks {
            let fname = task.src.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            self.progress.set_current(&fname, task.size);
            let mut attempt = 0;
            loop {
                match copy_verified(&task.src, &task.dst, &self.progress, opts) {
                    Ok(hash) => {
                        let _ = self.manifest.record(&FileRecord {
                            volume_uuid: &job.card_id,
                            rel_path: &task.rel_path,
                            size: task.size,
                            mtime: task.mtime,
                            hash: Some(hash),
                            dest_path: &task.dst,
                        });
                        self.progress.file_done();
                        copied.push(task);
                        break;
                    }
                    // 哈希不一致：删除临时文件重试 1 次
                    Err(CopyError::HashMismatch(_)) if attempt == 0 => attempt += 1,
                    Err(CopyError::HashMismatch(_)) => {
                        bad.push(fname.clone());
                        break;
                    }
                    Err(CopyError::Cancelled(_)) => {
                        if let Some(id) = job_id {
                            let _ = self.manifest.job_finish(id, "cancelled", copied.len() as u32, plan.skipped.len() as u32, 0, None, None);
                        }
                        self.progress.reset();
                        return Outcome::Silent;
                    }
                    Err(CopyError::Source(e)) => {
                        drop(sleep_guard);
                        let reason = if crate::detector::is_card(&job.mount) {
                            FailReason::Io { message: format!("{fname}：{e}") }
                        } else {
                            FailReason::CardRemoved
                        };
                        return self.fail(job, job_id, reason);
                    }
                    Err(CopyError::Dest(e)) => {
                        drop(sleep_guard);
                        if !self.config().target_connected() {
                            if let Some(id) = job_id {
                                let _ = self.manifest.job_finish(id, "waiting", copied.len() as u32, 0, 0, Some("目标盘已断开"), None);
                            }
                            return self.wait_for_target(job, &cfg);
                        }
                        if e.raw_os_error() == Some(libc::ENOSPC) {
                            let available = crate::platform::free_space(&target).unwrap_or(0);
                            let pending: u64 = plan.tasks.iter().filter(|t| !copied.iter().any(|c| c.rel_path == t.rel_path)).map(|t| t.size).sum();
                            return self.fail(
                                job,
                                job_id,
                                FailReason::NoSpace {
                                    need_bytes: (pending + planner::SPACE_RESERVE).saturating_sub(available),
                                    available_bytes: available,
                                    pending_bytes: pending,
                                },
                            );
                        }
                        return self.fail(job, job_id, FailReason::Io { message: format!("{fname}：{e}") });
                    }
                }
            }
        }

        // 收尾核对：每个正式文件都在且大小一致
        self.progress.set_state(JobState::Verifying);
        for t in &copied {
            let ok = std::fs::metadata(&t.dst).map(|m| m.len() == t.size).unwrap_or(false);
            if !ok {
                bad.push(t.src.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
            }
        }
        drop(sleep_guard);
        if !bad.is_empty() {
            return self.fail(job, job_id, FailReason::VerifyFailed { count: bad.len() as u32, first: bad[0].clone() });
        }

        let (ejected, eject_error) = self.maybe_eject(job, &cfg);
        let snap = self.progress.snapshot();
        if let Some(id) = job_id {
            let _ = self.manifest.job_finish(
                id,
                "done",
                copied.len() as u32,
                plan.skipped.len() as u32,
                snap.bytes_done,
                eject_error.as_deref(),
                Some(&display_dir),
            );
        }
        self.progress.finish(true, ejected, eject_error.clone());
        self.progress.set_state(JobState::Done);
        self.completed.insert(job.card_id.clone(), Instant::now());
        let _ = self.notify.send(Notice::Done {
            card: job.name.clone(),
            files: copied.len() as u32,
            ejected,
            eject_error,
            auto_eject: cfg.auto_eject,
        });
        Outcome::Finished
    }

    /// 只有所有文件都是「已校验」或「已跳过」时才会走到这里
    fn maybe_eject(&self, job: &Job, cfg: &Config) -> (bool, Option<String>) {
        if !cfg.auto_eject {
            return (false, None);
        }
        self.progress.set_state(JobState::Ejecting);
        match self.ejector.eject(&job.mount, &job.bsd) {
            Ok(()) => (true, None),
            Err(e) => (false, Some(e.to_string())),
        }
    }
}

/// 不弹卡的 Ejector，CLI 与测试用
pub struct NoopEjector;

impl Ejector for NoopEjector {
    fn eject(&self, _mount: &Path, _bsd: &str) -> Result<(), crate::platform::EjectError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;
    use std::fs;

    fn setup(card: &Path, target: &Path) -> (Runner, crossbeam_channel::Receiver<Notice>, Arc<Progress>) {
        fs::create_dir_all(card.join("DCIM/100CANON")).unwrap();
        let cfg = Arc::new(RwLock::new(Config { target_path: Some(target.to_path_buf()), ..Default::default() }));
        let progress = Arc::new(Progress::new());
        let (ntx, nrx) = unbounded();
        let r = Runner::new(cfg, progress.clone(), Manifest::open_in_memory().unwrap(), Box::new(NoopEjector), ntx);
        (r, nrx, progress)
    }

    fn job(card: &Path) -> Job {
        Job { card_id: "CARD-1".into(), bsd: "disk9s1".into(), mount: card.to_path_buf(), name: "EOS_R5".into(), capacity: 0 }
    }

    #[test]
    fn full_run_then_incremental() {
        let card = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let (mut r, nrx, progress) = setup(card.path(), target.path());
        fs::write(card.path().join("DCIM/100CANON/IMG_0001.CR3"), vec![1u8; 5000]).unwrap();
        fs::write(card.path().join("DCIM/100CANON/IMG_0001.XMP"), b"<x/>").unwrap();

        assert!(matches!(r.run_job(&job(card.path())), Outcome::Finished));
        let s = progress.snapshot();
        assert_eq!(s.state, JobState::Done);
        assert_eq!(s.files_done, 2);
        assert!(s.ejected);
        let notices: Vec<_> = nrx.try_iter().collect();
        assert!(matches!(notices[0], Notice::Started { files: 2, .. }));
        assert_eq!(notices[1].body(), "已备份 2 个文件并全部校验，卡已弹出");

        // 第二次插入：全部跳过，且同一会话内静默
        assert!(matches!(r.run_job(&job(card.path())), Outcome::Silent));
        assert!(nrx.try_iter().next().is_none());

        // 新增一个文件后只拷新文件
        fs::write(card.path().join("DCIM/100CANON/IMG_0002.CR3"), vec![2u8; 100]).unwrap();
        r.run_job(&job(card.path()));
        assert_eq!(progress.snapshot().files_done, 1);
    }

    #[test]
    fn waits_for_missing_target() {
        let card = tempfile::tempdir().unwrap();
        let (mut r, nrx, progress) = setup(card.path(), Path::new("/nonexistent/offload-target"));
        fs::write(card.path().join("DCIM/100CANON/A.JPG"), b"x").unwrap();
        assert!(matches!(r.run_job(&job(card.path())), Outcome::Waiting));
        assert_eq!(progress.state(), JobState::WaitingForTarget);
        assert!(nrx.try_recv().unwrap().body().contains("接入后将自动继续"));
    }

    /// 两张卡依次插入、内容各一个文件；返回卡的临时目录（需保持存活）
    fn two_cards(r: &mut Runner) -> Vec<tempfile::TempDir> {
        let cards: Vec<_> = (0..2).map(|_| tempfile::tempdir().unwrap()).collect();
        for (i, c) in cards.iter().enumerate() {
            fs::create_dir_all(c.path().join("DCIM/100CANON")).unwrap();
            fs::write(c.path().join(format!("DCIM/100CANON/IMG_000{i}.JPG")), vec![i as u8; 100]).unwrap();
            let job = Job { card_id: format!("CARD-{i}"), bsd: format!("disk{i}s1"), mount: c.path().to_path_buf(), name: format!("卡{i}"), capacity: 0 };
            r.handle(Command::Enqueue(job));
        }
        cards
    }

    fn files_in(dir: &Path) -> usize {
        walkdir::WalkDir::new(dir).into_iter().flatten().filter(|e| e.file_type().is_file()).count()
    }

    #[test]
    fn every_waiting_card_resumes() {
        let target = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let (mut r, _nrx, progress) = setup(scratch.path(), Path::new("/nonexistent/offload-target"));
        let _cards = two_cards(&mut r);
        while r.run_next() {}
        assert_eq!(r.waiting.len(), 2);
        assert_eq!(progress.state(), JobState::WaitingForTarget);

        r.cfg.write().unwrap().target_path = Some(target.path().to_path_buf());
        r.handle(Command::TargetMounted);
        while r.run_next() {}
        assert!(r.waiting.is_empty());
        assert_eq!(files_in(target.path()), 2);
    }

    #[test]
    fn removing_waiting_card_shows_the_other() {
        let scratch = tempfile::tempdir().unwrap();
        let (mut r, _nrx, progress) = setup(scratch.path(), Path::new("/nonexistent/offload-target"));
        let _cards = two_cards(&mut r);
        while r.run_next() {}
        assert_eq!(progress.snapshot().card_name.as_deref(), Some("卡1"));

        r.handle(Command::CardRemoved { bsd: "disk1s1".into() });
        assert_eq!(progress.state(), JobState::WaitingForTarget);
        assert_eq!(progress.snapshot().card_name.as_deref(), Some("卡0"));

        r.handle(Command::CardRemoved { bsd: "disk0s1".into() });
        assert_eq!(progress.state(), JobState::Idle);
    }

    #[test]
    fn retry_requeues_every_failed_card() {
        let target = tempfile::tempdir().unwrap();
        // 目标路径是个普通文件：建目录失败
        let blocker = target.path().join("blocker");
        fs::write(&blocker, b"x").unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let (mut r, _nrx, progress) = setup(scratch.path(), &blocker);
        let _cards = two_cards(&mut r);
        while r.run_next() {}
        assert_eq!(r.failed.len(), 2);
        assert_eq!(progress.state(), JobState::Failed);

        let ok = target.path().join("ok");
        r.cfg.write().unwrap().target_path = Some(ok.clone());
        r.handle(Command::Retry);
        while r.run_next() {}
        assert!(r.failed.is_empty());
        assert_eq!(files_in(&ok), 2);
    }

    #[test]
    fn no_target_fails() {
        let card = tempfile::tempdir().unwrap();
        let (mut r, _nrx, progress) = setup(card.path(), Path::new("/x"));
        r.cfg.write().unwrap().target_path = None;
        fs::write(card.path().join("DCIM/100CANON/A.JPG"), b"x").unwrap();
        assert!(matches!(r.run_job(&job(card.path())), Outcome::Waiting));
        assert!(matches!(progress.snapshot().fail, Some(FailReason::NoTarget)));
    }

    #[test]
    fn notice_texts_match_spec() {
        let n = Notice::Started { card: "EOS_DIGITAL".into(), files: 312, bytes: 48_200_000_000 };
        assert_eq!(n.title(), "正在备份「EOS_DIGITAL」");
        assert_eq!(n.body(), "312 个新文件，48.2 GB");
        let f = Notice::Failed {
            card: "x".into(),
            reason: FailReason::NoSpace { need_bytes: 12_400_000_000, available_bytes: 0, pending_bytes: 0 },
        };
        assert_eq!(f.body(), "备份中断：目标盘空间不足（还需 12.4 GB），卡未弹出");
        let v = Notice::Failed { card: "x".into(), reason: FailReason::VerifyFailed { count: 2, first: "IMG_4833.CR3".into() } };
        assert_eq!(v.title(), "2 个文件校验未通过");
    }
}
