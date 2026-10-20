//! 调试工具：offload-cli watch | plan | copy | backup

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use crossbeam_channel::unbounded;
use offload_core::copier::{copy_verified, CopyOptions};
use offload_core::job::{Command, NoopEjector, Runner};
use offload_core::manifest::{self, Manifest};
use offload_core::planner::{self, DateSource, PlanInput};
use offload_core::platform::{self, Ejector, PlatformEjector, PlatformWatcher, VolumeEvent, VolumeWatcher};
use offload_core::{fmt_bytes, Config, FolderLayout, JobState, Progress};

#[derive(Parser)]
#[command(name = "offload-cli", version, about = "Offload 调试工具")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 打印每个磁盘事件的 UUID、路径、BSD 名、是否有 DCIM
    Watch,
    /// 扫描卡并生成拷贝计划，只打印不执行
    Plan {
        card: PathBuf,
        target: PathBuf,
        #[arg(long, value_enum, default_value = "date")]
        layout: Layout,
        /// 使用指定的 manifest（默认使用应用的 manifest）
        #[arg(long)]
        manifest: Option<PathBuf>,
        /// 打印每个文件
        #[arg(short, long)]
        verbose: bool,
        /// 同时备份代理文件（LRF、LRV、Sony SUB）
        #[arg(long)]
        proxies: bool,
    },
    /// 拷贝单个文件：流水线、xxh3、.part、F_FULLFSYNC、回读校验、rename
    Copy {
        src: PathBuf,
        dst: PathBuf,
        /// 额外回读源文件
        #[arg(long)]
        paranoid: bool,
    },
    /// 对一张已挂载的卡执行完整备份
    Backup {
        card: PathBuf,
        target: PathBuf,
        #[arg(long, value_enum, default_value = "date")]
        layout: Layout,
        #[arg(long)]
        manifest: Option<PathBuf>,
        /// 完成后弹出卡
        #[arg(long)]
        eject: bool,
        /// 同时备份代理文件（LRF、LRV、Sony SUB）
        #[arg(long)]
        proxies: bool,
    },
    /// 列出外置卷
    Volumes,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Layout {
    Date,
    Camera,
    Card,
}

impl From<Layout> for FolderLayout {
    fn from(l: Layout) -> Self {
        match l {
            Layout::Date => FolderLayout::Date,
            Layout::Camera => FolderLayout::Camera,
            Layout::Card => FolderLayout::Card,
        }
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()))
        .with_writer(std::io::stderr)
        .init();
    match Cli::parse().cmd {
        Cmd::Watch => watch(),
        Cmd::Plan { card, target, layout, manifest, verbose, proxies } => plan(card, target, layout.into(), manifest, verbose, proxies),
        Cmd::Copy { src, dst, paranoid } => copy(src, dst, paranoid),
        Cmd::Backup { card, target, layout, manifest, eject, proxies } => backup(card, target, layout.into(), manifest, eject, proxies),
        Cmd::Volumes => {
            for v in platform::list_external_volumes() {
                println!(
                    "{:<24} {:>10} 可用 / {:>10}  uuid={}  bsd={}  {}",
                    v.name,
                    fmt_bytes(v.free_bytes),
                    fmt_bytes(v.total_bytes),
                    v.uuid.as_deref().unwrap_or("-"),
                    v.bsd.as_deref().unwrap_or("-"),
                    if v.is_card { "DCIM" } else { "" }
                );
            }
            Ok(())
        }
    }
}

fn watch() -> Result<()> {
    let (tx, rx) = unbounded();
    PlatformWatcher.spawn(tx)?;
    eprintln!("监听磁盘事件中，Ctrl-C 退出…");
    let t0 = Instant::now();
    for ev in rx {
        let ts = t0.elapsed().as_secs_f32();
        match ev {
            VolumeEvent::Mounted { bsd, uuid, path, name, size } => {
                let dcim = path.join("DCIM").is_dir();
                println!(
                    "[{ts:8.2}] MOUNT   bsd={bsd:<10} uuid={:<36} dcim={:<5} size={:<9} name={name:?} path={}",
                    uuid.as_deref().unwrap_or("-"),
                    dcim,
                    fmt_bytes(size),
                    path.display()
                );
            }
            VolumeEvent::Unmounted { bsd, uuid } => {
                println!("[{ts:8.2}] UNMOUNT bsd={bsd:<10} uuid={}", uuid.as_deref().unwrap_or("-"));
            }
        }
    }
    Ok(())
}

fn open_manifest(p: Option<PathBuf>) -> Result<Manifest> {
    let path = p.unwrap_or_else(|| {
        let path = manifest::default_path();
        manifest::migrate_legacy(&path);
        path
    });
    Manifest::open(&path).with_context(|| format!("打开 manifest 失败：{}", path.display()))
}

fn plan(card: PathBuf, target: PathBuf, layout: FolderLayout, manifest: Option<PathBuf>, verbose: bool, proxies: bool) -> Result<()> {
    if !card.join("DCIM").is_dir() {
        bail!("{} 下没有 DCIM，不像相机卡", card.display());
    }
    let job = offload_core::detector::job_for_path(card.clone()).context("无法识别卡")?;
    let m = open_manifest(manifest)?;
    let t0 = Instant::now();
    let p = planner::plan(&PlanInput {
        card_root: &card,
        card_id: &job.card_id,
        card_name: &job.name,
        target_root: &target,
        layout,
        manifest: &m,
        include_proxies: proxies,
    })?;
    let elapsed = t0.elapsed();

    if verbose {
        for g in &p.groups {
            let src = match g.source {
                DateSource::Exif => "EXIF ",
                DateSource::Mtime => "mtime",
            };
            println!("{} [{src}] {}", g.date, g.members.join("  +  "));
        }
        println!();
        for t in &p.tasks {
            println!("COPY  {}  →  {}", t.rel_path, t.dst.display());
        }
        for s in &p.skipped {
            println!("SKIP  {}  ({}{})", s.rel_path, s.dst.display(), if s.needs_record { "，补记" } else { "" });
        }
        println!();
    }
    println!("卡：{}（id {}）", job.name, job.card_id);
    println!("文件组：{}", p.groups.len());
    println!("待拷贝：{} 个文件，{}", p.tasks.len(), fmt_bytes(p.bytes_total));
    println!("跳过：{} 个文件", p.skipped.len());
    println!("重命名：{} 个文件", p.renamed);
    println!("目标目录：{} 个", p.dest_dirs.len());
    match planner::check_space(p.bytes_total, &target) {
        Ok(()) => println!("空间：足够"),
        Err(s) => println!("空间：不足，还需 {}", fmt_bytes(s.need)),
    }
    println!("规划耗时：{:.2?}", elapsed);
    Ok(())
}

fn copy(src: PathBuf, dst: PathBuf, paranoid: bool) -> Result<()> {
    let size = std::fs::metadata(&src)?.len();
    let prog = Arc::new(Progress::new());
    prog.begin("copy", 0, None);
    prog.set_plan(1, size, 0, None);
    prog.set_state(JobState::Copying);

    // Ctrl-C：取消，只留下 .offload-part 被清理
    {
        let prog = prog.clone();
        ctrlc_cancel(move || prog.cancel());
    }
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ticker = {
        let prog = prog.clone();
        let done = done.clone();
        std::thread::spawn(move || {
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                let s = prog.snapshot();
                eprint!(
                    "\r{:>5.1}%  {}/s  {} / {}   ",
                    s.bytes_done as f64 * 100.0 / s.bytes_total.max(1) as f64,
                    fmt_bytes(s.speed_bps),
                    fmt_bytes(s.bytes_done),
                    fmt_bytes(s.bytes_total)
                );
                std::thread::sleep(Duration::from_millis(200));
            }
        })
    };
    let t0 = Instant::now();
    let r = copy_verified(&src, &dst, &prog, CopyOptions { verify: true, paranoid });
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = ticker.join();
    eprintln!();
    let h = r?;
    let secs = t0.elapsed().as_secs_f64();
    println!("xxh3-128 {h:032x}");
    println!("{} 用时 {secs:.2}s，平均 {}/s（含回读校验）", fmt_bytes(size), fmt_bytes((size as f64 / secs) as u64));
    Ok(())
}

fn backup(card: PathBuf, target: PathBuf, layout: FolderLayout, manifest: Option<PathBuf>, eject: bool, proxies: bool) -> Result<()> {
    let job = offload_core::detector::job_for_path(card.clone()).context("该路径下没有 DCIM")?;
    let target_volume_uuid = platform::volume_info_nearest(&target).and_then(|v| v.uuid);
    let cfg = Arc::new(RwLock::new(Config {
        target_path: Some(target),
        target_volume_uuid,
        auto_eject: eject,
        folder_layout: layout,
        backup_proxies: proxies,
        ..Default::default()
    }));
    let progress = Arc::new(Progress::new());
    let (ntx, nrx) = unbounded();
    let ejector: Box<dyn Ejector> = if eject { Box::new(PlatformEjector) } else { Box::new(NoopEjector) };
    let runner = Runner::new(cfg, progress.clone(), open_manifest(manifest)?, ejector, ntx);
    let (ctx, crx) = unbounded();
    ctx.send(Command::Enqueue(job))?;
    let handle = std::thread::spawn(move || runner.run(crx));
    {
        let progress = progress.clone();
        ctrlc_cancel(move || progress.cancel());
    }

    let mut seen_active = false;
    loop {
        for n in nrx.try_iter() {
            eprintln!("\n【{}】{}", n.title(), n.body());
        }
        let s = progress.snapshot();
        if s.state.is_active() {
            seen_active = true;
            eprint!(
                "\r{:?}  {}/{} 个文件  {} / {}  {}/s   ",
                s.state,
                s.files_done,
                s.files_total,
                fmt_bytes(s.bytes_done),
                fmt_bytes(s.bytes_total),
                fmt_bytes(s.speed_bps)
            );
        } else if seen_active || matches!(s.state, JobState::Done | JobState::Failed | JobState::WaitingForTarget) {
            for n in nrx.try_iter() {
                eprintln!("\n【{}】{}", n.title(), n.body());
            }
            eprintln!();
            println!("状态：{:?}", s.state);
            if let Some(e) = s.last_error {
                println!("错误：{e}");
            }
            println!("已拷贝 {} 个，跳过 {} 个，{}", s.files_done, s.files_skipped, fmt_bytes(s.bytes_done));
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    ctx.send(Command::Shutdown)?;
    let _ = handle.join();
    Ok(())
}

/// 极简 SIGINT 处理：第一次取消任务，不依赖额外 crate
fn ctrlc_cancel(f: impl Fn() + Send + 'static) {
    use std::sync::OnceLock;
    static HANDLER: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
    let f = std::sync::Mutex::new(f);
    let _ = HANDLER.set(Box::new(move || (f.lock().unwrap())()));
    extern "C" fn on_sigint(_: libc_int) {
        if let Some(h) = HANDLER.get() {
            h();
        }
    }
    #[allow(non_camel_case_types)]
    type libc_int = i32;
    extern "C" {
        fn signal(sig: libc_int, handler: extern "C" fn(libc_int)) -> usize;
    }
    unsafe {
        signal(2, on_sigint);
    }
}
