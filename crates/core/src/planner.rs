//! 规划器：扫描卡、取日期、算目标路径、判定跳过或重命名、检查空间。
//!
//! 目标路径由「归档模板 + 拍摄日期等信息 + 原文件名」唯一确定，默认 `<目标根>/YYYY/MM/DD/<原文件名>`，
//! 同一张卡重复插入时每个文件算出同一路径，增量判断因此成立。

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;
use chrono::{DateTime, Local, NaiveDate};
use walkdir::WalkDir;

use crate::copier::{part_path_with, LEGACY_PART_SUFFIX, PART_SUFFIX};
use crate::layout::{FolderTemplate, Vars};
use crate::manifest::Manifest;

/// 读 EXIF DateTimeOriginal 的图片与 RAW
const EXIF_EXTS: &[&str] = &[
    "jpg", "jpeg", "heic", "heif", "hif", "tif", "tiff", "dng", "cr2", "cr3", "nef", "nrw", "arw", "srf", "sr2",
    "raf", "orf", "rw2", "pef", "srw",
];
/// 其他需要备份的媒体（视频按文件时间）
const MEDIA_EXTS: &[&str] = &[
    "png", "gpr", "crw", "x3f", "3fr", "iiq", "mp4", "mov", "m4v", "avi", "mts", "m2ts", "mxf", "insv", "insp",
    "braw", "r3d", "crm", "wav", "mp3", "lrv", "360",
];
/// 附属文件：跟随同名主文件
const SIDECAR_EXTS: &[&str] = &["xmp", "thm", "xml", "lrf"];
/// 不备份的目录（按目录名，不区分大小写，任意层级）
const IGNORED_DIRS: &[&str] = &[
    "System Volume Information",
    "$RECYCLE.BIN",
    // Canon：DCIM 下的目录索引 .CTG
    "CANONMSC",
    // Sony XAVC：片段缩略图
    "THMBNL",
];
/// 不备份的卡索引文件：每次拍摄都会被相机重写
const IGNORED_FILES: &[&str] = &["MEDIAPRO.XML", "CUEUP.XML"];
/// 代理与低码率预览（DJI LRF、GoPro / Insta360 LRV），可由原片重新生成
const PROXY_EXTS: &[&str] = &["lrf", "lrv"];
/// 代理录制目录（相对卡根目录）
const PROXY_DIRS: &[&str] = &["PRIVATE/M4ROOT/SUB"];
/// 同名同大小时抽样比对的头尾长度
const SAMPLE_LEN: u64 = 64 * 1024;

#[derive(Debug, Clone)]
pub struct CardFile {
    pub abs: PathBuf,
    /// 相对卡根目录，用 `/` 分隔
    pub rel: String,
    pub name: String,
    pub stem: String,
    pub ext: String,
    pub size: u64,
    /// Unix 秒
    pub mtime: i64,
    pub mtime_local: Option<NaiveDate>,
}

impl CardFile {
    fn ext_lower(&self) -> String {
        self.ext.to_ascii_lowercase()
    }
    pub fn is_sidecar(&self) -> bool {
        SIDECAR_EXTS.contains(&self.ext_lower().as_str())
    }
    fn has_exif(&self) -> bool {
        EXIF_EXTS.contains(&self.ext_lower().as_str())
    }
}

/// 日期从哪里来
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateSource {
    Exif,
    Mtime,
}

#[derive(Debug, Clone)]
pub struct CopyTask {
    pub src: PathBuf,
    pub rel_path: String,
    pub dst: PathBuf,
    pub size: u64,
    pub mtime: i64,
}

#[derive(Debug, Clone)]
pub struct SkipItem {
    pub rel_path: String,
    pub dst: PathBuf,
    pub size: u64,
    pub mtime: i64,
    /// 规则 2：manifest 里没有，需要补记
    pub needs_record: bool,
}

/// 一组文件（主文件 + 附属文件）的规划结果，供 CLI 打印
#[derive(Debug, Clone)]
pub struct GroupInfo {
    pub date: NaiveDate,
    pub source: DateSource,
    pub members: Vec<String>,
}

#[derive(Debug, Default)]
pub struct Plan {
    pub tasks: Vec<CopyTask>,
    pub skipped: Vec<SkipItem>,
    pub groups: Vec<GroupInfo>,
    pub bytes_total: u64,
    pub dest_dirs: BTreeSet<PathBuf>,
    /// 目标路径冲突、加了后缀的文件数
    pub renamed: u32,
}

impl Plan {
    /// 弹窗里展示的「备份位置」：只落在一天时显示那天的目录，否则显示根目录
    pub fn display_dir(&self, target_root: &Path) -> PathBuf {
        if self.dest_dirs.len() == 1 {
            self.dest_dirs.iter().next().cloned().unwrap_or_else(|| target_root.to_path_buf())
        } else {
            target_root.to_path_buf()
        }
    }
}

pub struct PlanInput<'a> {
    pub card_root: &'a Path,
    pub card_id: &'a str,
    pub card_name: &'a str,
    pub target_root: &'a Path,
    pub layout: &'a FolderTemplate,
    pub manifest: &'a Manifest,
    /// 备份代理文件（LRF、LRV、Sony SUB 目录）
    pub include_proxies: bool,
}

/// 相对卡根目录的路径，用 `/` 分隔
fn rel_of(card_root: &Path, abs: &Path) -> String {
    let rel = abs.strip_prefix(card_root).unwrap_or(abs);
    rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}

fn ignored_entry(card_root: &Path, e: &walkdir::DirEntry, include_proxies: bool) -> bool {
    let n = e.file_name().to_string_lossy();
    if n.starts_with('.') {
        return true;
    }
    if !e.file_type().is_dir() {
        return IGNORED_FILES.iter().any(|x| x.eq_ignore_ascii_case(&n));
    }
    if IGNORED_DIRS.iter().any(|x| x.eq_ignore_ascii_case(&n)) {
        return true;
    }
    !include_proxies && {
        let rel = rel_of(card_root, e.path());
        PROXY_DIRS.iter().any(|x| x.eq_ignore_ascii_case(&rel))
    }
}

/// 扫描卡上需要备份的文件：DCIM 下除黑名单外的全部文件，以及其他目录里的已知媒体与附属文件
pub fn scan_card(card_root: &Path, include_proxies: bool) -> Result<Vec<CardFile>> {
    let mut out = Vec::new();
    let walker = WalkDir::new(card_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !ignored_entry(card_root, e, include_proxies));
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "scan error");
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let abs = entry.path().to_path_buf();
        let rel = rel_of(card_root, &abs);
        let name = entry.file_name().to_string_lossy().into_owned();
        let (stem, ext) = match name.rfind('.') {
            Some(i) if i > 0 => (name[..i].to_string(), name[i + 1..].to_string()),
            _ => (name.clone(), String::new()),
        };
        let in_dcim = rel.split('/').next().map(|s| s.eq_ignore_ascii_case("DCIM")).unwrap_or(false);
        let e = ext.to_ascii_lowercase();
        let known = EXIF_EXTS.contains(&e.as_str()) || MEDIA_EXTS.contains(&e.as_str()) || SIDECAR_EXTS.contains(&e.as_str());
        if (!in_dcim && !known) || (!include_proxies && PROXY_EXTS.contains(&e.as_str())) {
            continue;
        }
        let md = entry.metadata()?;
        let modified = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let mtime = modified.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        out.push(CardFile {
            abs,
            rel,
            name,
            stem,
            ext,
            size: md.len(),
            mtime,
            // exFAT 上的 mtime 是相机写入的本地时间
            mtime_local: Some(DateTime::<Local>::from(modified).date_naive()),
        });
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

/// 按「目录 + 文件名主干」分组。附属文件找同名主文件；
/// 找不到时找主干是其前缀的主文件（Sony C0001.MP4 ↔ C0001M01.XML）。
pub fn group_files(files: Vec<CardFile>) -> Vec<Vec<CardFile>> {
    let mut by_dir: BTreeMap<String, Vec<CardFile>> = BTreeMap::new();
    for f in files {
        let dir = f.rel.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
        by_dir.entry(dir).or_default().push(f);
    }
    let mut groups = Vec::new();
    for (_, files) in by_dir {
        let (sidecars, primaries): (Vec<_>, Vec<_>) = files.into_iter().partition(|f| f.is_sidecar());
        let mut index: BTreeMap<String, Vec<CardFile>> = BTreeMap::new();
        for p in primaries {
            index.entry(p.stem.to_ascii_lowercase()).or_default().push(p);
        }
        let mut orphans = Vec::new();
        for s in sidecars {
            let key = s.stem.to_ascii_lowercase();
            if let Some(g) = index.get_mut(&key) {
                g.push(s);
                continue;
            }
            let best = index.keys().filter(|k| key.starts_with(k.as_str())).max_by_key(|k| k.len()).cloned();
            match best {
                Some(k) => index.get_mut(&k).unwrap().push(s),
                None => orphans.push(s),
            }
        }
        groups.extend(index.into_values());
        // 没有主文件的附属文件按自身 mtime 各自成组
        let mut orphan_index: BTreeMap<String, Vec<CardFile>> = BTreeMap::new();
        for o in orphans {
            orphan_index.entry(o.stem.to_ascii_lowercase()).or_default().push(o);
        }
        groups.extend(orphan_index.into_values());
    }
    groups
}

#[derive(Debug, Default, Clone)]
pub struct ExifInfo {
    pub date: Option<NaiveDate>,
    pub model: Option<String>,
}

/// 读 EXIF 拍摄日期与相机型号
pub fn read_exif_info(path: &Path) -> ExifInfo {
    use nom_exif::ExifTag;
    let Ok(exif) = nom_exif::read_exif(path) else {
        return ExifInfo::default();
    };
    let date = [ExifTag::DateTimeOriginal, ExifTag::CreateDate, ExifTag::ModifyDate]
        .into_iter()
        .find_map(|t| exif.get(t).and_then(|v| v.as_datetime()))
        .map(|d| d.into_naive().date());
    let model = exif.get(ExifTag::Model).and_then(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    ExifInfo { date, model }
}

/// 一组文件共用的日期：组内任一图片的 EXIF 日期，否则主文件的 mtime
fn group_date(group: &[CardFile], exif: &mut impl FnMut(&Path) -> ExifInfo) -> (NaiveDate, DateSource, Option<String>) {
    let mut model = None;
    for f in group.iter().filter(|f| f.has_exif()) {
        let info = exif(&f.abs);
        if model.is_none() {
            model = info.model.clone();
        }
        if let Some(d) = info.date {
            return (d, DateSource::Exif, model.or(info.model));
        }
    }
    let primary = group.iter().find(|f| !f.is_sidecar()).unwrap_or(&group[0]);
    let d = primary.mtime_local.unwrap_or_else(|| Local::now().date_naive());
    (d, DateSource::Mtime, model)
}

/// 比对两个同样大小的文件的头尾各 64 KB。不同机身拍出的同名同大小文件
/// （如固定大小的无压缩 RAW）EXIF 里的时间、序列号必然不同；读不了按不同处理。
fn same_content(a: &Path, b: &Path, size: u64) -> bool {
    fn sample(p: &Path, size: u64) -> std::io::Result<Vec<u8>> {
        let mut f = fs::File::open(p)?;
        let head = SAMPLE_LEN.min(size);
        let mut buf = vec![0u8; head as usize];
        f.read_exact(&mut buf)?;
        let tail = SAMPLE_LEN.min(size - head);
        if tail > 0 {
            f.seek(SeekFrom::Start(size - tail))?;
            let mut t = vec![0u8; tail as usize];
            f.read_exact(&mut t)?;
            buf.extend_from_slice(&t);
        }
        Ok(buf)
    }
    matches!((sample(a, size), sample(b, size)), (Ok(x), Ok(y)) if x == y)
}

fn name_with_suffix(f: &CardFile, n: u32) -> String {
    match (n, f.ext.is_empty()) {
        (0, _) => f.name.clone(),
        (_, true) => format!("{}_{n}", f.stem),
        (_, false) => format!("{}_{n}.{}", f.stem, f.ext),
    }
}

/// 生成拷贝计划（只读，不改动目标盘上的正式文件）
pub fn plan(input: &PlanInput) -> Result<Plan> {
    plan_with(input, &mut read_exif_info)
}

pub fn plan_with(input: &PlanInput, exif: &mut impl FnMut(&Path) -> ExifInfo) -> Result<Plan> {
    let files = scan_card(input.card_root, input.include_proxies)?;
    let groups = group_files(files);

    // 先算每组日期与相机，视频没有 EXIF 时按卡上出现最多的相机归档
    let mut dated = Vec::with_capacity(groups.len());
    let mut model_count: HashMap<String, usize> = HashMap::new();
    for g in groups {
        let (date, source, model) = group_date(&g, exif);
        if let Some(m) = &model {
            *model_count.entry(m.clone()).or_default() += 1;
        }
        dated.push((g, date, source, model));
    }
    let main_model = model_count.into_iter().max_by_key(|(_, c)| *c).map(|(m, _)| m);

    let mut plan = Plan::default();
    let mut reserved: HashSet<PathBuf> = HashSet::new();

    for (group, date, source, model) in dated {
        let camera = if input.layout.uses_camera() {
            model.clone().or_else(|| main_model.clone()).unwrap_or_else(|| "未知相机".into())
        } else {
            String::new()
        };
        let dir = input.layout.render(input.target_root, &Vars { date, camera: &camera, card: input.card_name });

        plan.groups.push(GroupInfo { date, source, members: group.iter().map(|f| f.rel.clone()).collect() });

        // 规则 1：manifest 有记录且目标文件仍在 → 跳过
        let mut pending = Vec::new();
        for f in group {
            match input.manifest.lookup(input.card_id, &f.rel, f.size, f.mtime)? {
                Some(d) if d.exists() => {
                    reserved.insert(d.clone());
                    plan.skipped.push(SkipItem { rel_path: f.rel, dst: d, size: f.size, mtime: f.mtime, needs_record: false });
                }
                _ => pending.push(f),
            }
        }
        if pending.is_empty() {
            continue;
        }

        // 规则 2、3：同名同大小且头尾内容一致 → 跳过并补记；否则加后缀 _1、_2…（整组共用同一后缀）
        let mut n = 0u32;
        let decisions = loop {
            let mut ok = true;
            let mut ds = Vec::with_capacity(pending.len());
            for f in &pending {
                let cand = dir.join(name_with_suffix(f, n));
                if reserved.contains(&cand) {
                    ok = false;
                    break;
                }
                match fs::metadata(&cand) {
                    Ok(md) if md.is_file() && md.len() == f.size && same_content(&f.abs, &cand, f.size) => {
                        ds.push((cand, true))
                    }
                    Ok(_) => {
                        ok = false;
                        break;
                    }
                    Err(_) => ds.push((cand, false)),
                }
            }
            if ok {
                break ds;
            }
            n += 1;
        };
        if n > 0 {
            plan.renamed += pending.len() as u32;
        }
        for (f, (dst, skip)) in pending.into_iter().zip(decisions) {
            reserved.insert(dst.clone());
            if skip {
                plan.skipped.push(SkipItem { rel_path: f.rel, dst, size: f.size, mtime: f.mtime, needs_record: true });
            } else {
                plan.bytes_total += f.size;
                plan.dest_dirs.insert(dir.clone());
                plan.tasks.push(CopyTask { src: f.abs, rel_path: f.rel, dst, size: f.size, mtime: f.mtime });
            }
        }
    }
    Ok(plan)
}

/// 清理上次中断留下的 .offload-part（以及改名前的 .photosync-part）
pub fn clean_stale_parts(plan: &Plan) {
    for t in &plan.tasks {
        for suffix in [PART_SUFFIX, LEGACY_PART_SUFFIX] {
            let p = part_path_with(&t.dst, suffix);
            if p.exists() {
                let _ = fs::remove_file(p);
            }
        }
    }
}

/// 预留 64 MB，避免把目标盘写满
pub const SPACE_RESERVE: u64 = 64 * 1000 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpaceShortage {
    /// 还需多少空间
    pub need: u64,
    pub available: u64,
    pub pending: u64,
}

/// 开拷前确认目标剩余空间大于待拷总量
pub fn check_space(bytes_total: u64, target_root: &Path) -> Result<(), SpaceShortage> {
    if bytes_total == 0 {
        return Ok(());
    }
    let probe = target_root.ancestors().find(|p| p.exists()).unwrap_or(target_root);
    let Some(available) = crate::platform::free_space(probe) else {
        return Ok(());
    };
    let required = bytes_total + SPACE_RESERVE;
    if available >= required {
        Ok(())
    } else {
        Err(SpaceShortage { need: required - available, available, pending: bytes_total })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FolderLayout;
    use filetime::FileTime;

    fn preset(l: FolderLayout) -> FolderTemplate {
        FolderTemplate::parse(l.preset().unwrap()).unwrap()
    }

    fn touch(root: &Path, rel: &str, len: usize, ts: i64) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, vec![7u8; len]).unwrap();
        filetime::set_file_mtime(&p, FileTime::from_unix_time(ts, 0)).unwrap();
    }

    fn no_exif(_: &Path) -> ExifInfo {
        ExifInfo::default()
    }

    // 2026-10-20 12:00 本地时间附近（避免时区跨天）
    fn noon(day: u32) -> i64 {
        use chrono::TimeZone;
        Local.with_ymd_and_hms(2026, 10, day, 12, 0, 0).unwrap().timestamp()
    }

    #[test]
    fn scan_ignores_hidden_and_system() {
        let d = tempfile::tempdir().unwrap();
        touch(d.path(), "DCIM/100CANON/IMG_0001.CR3", 10, noon(20));
        touch(d.path(), "DCIM/100CANON/._IMG_0001.CR3", 1, noon(20));
        touch(d.path(), ".Spotlight-V100/store.db", 1, noon(20));
        touch(d.path(), ".Trashes/501/x.jpg", 1, noon(20));
        touch(d.path(), "MISC/firmware.bin", 1, noon(20));
        touch(d.path(), "PRIVATE/M4ROOT/CLIP/C0001.MP4", 20, noon(20));
        let files = scan_card(d.path(), false).unwrap();
        let rels: Vec<_> = files.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(rels, vec!["DCIM/100CANON/IMG_0001.CR3", "PRIVATE/M4ROOT/CLIP/C0001.MP4"]);
    }

    #[test]
    fn sidecars_follow_primary() {
        let d = tempfile::tempdir().unwrap();
        touch(d.path(), "PRIVATE/M4ROOT/CLIP/C0001.MP4", 20, noon(19));
        touch(d.path(), "PRIVATE/M4ROOT/CLIP/C0001M01.XML", 2, noon(21));
        touch(d.path(), "DCIM/100CANON/IMG_0001.CR3", 10, noon(20));
        touch(d.path(), "DCIM/100CANON/IMG_0001.XMP", 2, noon(22));
        touch(d.path(), "DCIM/100CANON/IMG_0002.JPG", 10, noon(20));
        let groups = group_files(scan_card(d.path(), false).unwrap());
        assert_eq!(groups.len(), 3);
        let sony = groups.iter().find(|g| g[0].stem == "C0001").unwrap();
        assert_eq!(sony.len(), 2);

        let m = Manifest::open_in_memory().unwrap();
        let t = tempfile::tempdir().unwrap();
        let input = PlanInput { card_root: d.path(), card_id: "U", card_name: "EOS", target_root: t.path(), layout: &FolderTemplate::default(), manifest: &m, include_proxies: false };
        let p = plan_with(&input, &mut no_exif).unwrap();
        let dst = |rel: &str| p.tasks.iter().find(|t| t.rel_path == rel).unwrap().dst.strip_prefix(t.path()).unwrap().to_string_lossy().into_owned();
        // 附属文件跟随主文件的日期
        assert_eq!(dst("PRIVATE/M4ROOT/CLIP/C0001M01.XML"), "2026/10/19/C0001M01.XML");
        assert_eq!(dst("DCIM/100CANON/IMG_0001.XMP"), "2026/10/20/IMG_0001.XMP");
        assert_eq!(p.tasks.len(), 5);
    }

    #[test]
    fn exif_date_wins_over_mtime() {
        let d = tempfile::tempdir().unwrap();
        touch(d.path(), "DCIM/100CANON/IMG_0001.CR3", 10, noon(20));
        let m = Manifest::open_in_memory().unwrap();
        let t = tempfile::tempdir().unwrap();
        let input = PlanInput { card_root: d.path(), card_id: "U", card_name: "EOS", target_root: t.path(), layout: &preset(FolderLayout::Camera), manifest: &m, include_proxies: false };
        let mut fake = |_: &Path| ExifInfo { date: NaiveDate::from_ymd_opt(2025, 1, 2), model: Some("Canon EOS R5".into()) };
        let p = plan_with(&input, &mut fake).unwrap();
        assert!(p.tasks[0].dst.ends_with("Canon EOS R5/2025/01/02/IMG_0001.CR3"));
        assert_eq!(p.groups[0].source, DateSource::Exif);
    }

    #[test]
    fn date_then_camera_layout() {
        let d = tempfile::tempdir().unwrap();
        touch(d.path(), "DCIM/100CANON/IMG_0001.CR3", 10, noon(20));
        touch(d.path(), "DCIM/100CANON/IMG_0001.XMP", 2, noon(20));
        touch(d.path(), "DCIM/100CANON/MVI_0002.MP4", 10, noon(20));
        let m = Manifest::open_in_memory().unwrap();
        let t = tempfile::tempdir().unwrap();
        let layout = preset(FolderLayout::DateCamera);
        let input = PlanInput { card_root: d.path(), card_id: "U", card_name: "EOS", target_root: t.path(), layout: &layout, manifest: &m, include_proxies: false };
        let mut fake = |p: &Path| ExifInfo {
            date: NaiveDate::from_ymd_opt(2025, 1, 2),
            model: p.extension().is_some_and(|e| e == "CR3").then(|| "Canon EOS R5".into()),
        };
        let p = plan_with(&input, &mut fake).unwrap();
        let dst = |rel: &str| p.tasks.iter().find(|t| t.rel_path == rel).unwrap().dst.strip_prefix(t.path()).unwrap().to_string_lossy().into_owned();
        assert_eq!(dst("DCIM/100CANON/IMG_0001.CR3"), "2025/01/02/Canon EOS R5/IMG_0001.CR3");
        assert_eq!(dst("DCIM/100CANON/IMG_0001.XMP"), "2025/01/02/Canon EOS R5/IMG_0001.XMP");
        // 视频没有 EXIF：按 mtime 取日期，型号用这张卡上最常见的
        assert_eq!(dst("DCIM/100CANON/MVI_0002.MP4"), "2026/10/20/Canon EOS R5/MVI_0002.MP4");

        let layout = preset(FolderLayout::DateCard);
        let input = PlanInput { layout: &layout, ..input };
        let p = plan_with(&input, &mut no_exif).unwrap();
        assert!(p.tasks[0].dst.strip_prefix(t.path()).unwrap().starts_with("2026/10/20/EOS"));
    }

    #[test]
    fn skip_and_rename_rules() {
        let d = tempfile::tempdir().unwrap();
        touch(d.path(), "DCIM/100NIKON/DSC_0001.JPG", 10, noon(20));
        touch(d.path(), "DCIM/100NIKON/DSC_0002.JPG", 10, noon(20));
        touch(d.path(), "DCIM/100NIKON/DSC_0003.JPG", 10, noon(20));
        let t = tempfile::tempdir().unwrap();
        // 0001：目标已存在且大小相同 → 跳过并补记
        touch(t.path(), "2026/10/20/DSC_0001.JPG", 10, noon(20));
        // 0002：另一台相机的同名文件，大小不同 → 加后缀
        touch(t.path(), "2026/10/20/DSC_0002.JPG", 99, noon(20));
        let m = Manifest::open_in_memory().unwrap();
        let input = PlanInput { card_root: d.path(), card_id: "U", card_name: "N", target_root: t.path(), layout: &FolderTemplate::default(), manifest: &m, include_proxies: false };
        let p = plan_with(&input, &mut no_exif).unwrap();
        assert_eq!(p.skipped.len(), 1);
        assert!(p.skipped[0].needs_record);
        let names: Vec<_> = p.tasks.iter().map(|t| t.dst.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec!["DSC_0002_1.JPG", "DSC_0003.JPG"]);
        assert_eq!(p.renamed, 1);
        assert_eq!(p.bytes_total, 20);
        assert!(p.display_dir(t.path()).ends_with("2026/10/20"));
    }

    #[test]
    fn scan_skips_brand_junk_and_proxies() {
        let d = tempfile::tempdir().unwrap();
        for rel in [
            "DCIM/CANONMSC/M0100.CTG",
            "PRIVATE/M4ROOT/MEDIAPRO.XML",
            "PRIVATE/M4ROOT/CUEUP.XML",
            "PRIVATE/M4ROOT/THMBNL/C0001T01.JPG",
            "PRIVATE/M4ROOT/CLIP/C0001.MP4",
            "PRIVATE/M4ROOT/CLIP/C0001M01.XML",
            "PRIVATE/M4ROOT/SUB/C0001S03.MP4",
            "DCIM/100MEDIA/DJI_0001.MP4",
            "DCIM/100MEDIA/DJI_0001.LRF",
            "DCIM/100GOPRO/GX010001.MP4",
            "DCIM/100GOPRO/GL010001.LRV",
        ] {
            touch(d.path(), rel, 1, noon(20));
        }
        let rels = |proxies| scan_card(d.path(), proxies).unwrap().into_iter().map(|f| f.rel).collect::<Vec<_>>();
        assert_eq!(
            rels(false),
            vec!["DCIM/100GOPRO/GX010001.MP4", "DCIM/100MEDIA/DJI_0001.MP4", "PRIVATE/M4ROOT/CLIP/C0001.MP4", "PRIVATE/M4ROOT/CLIP/C0001M01.XML"]
        );
        assert_eq!(
            rels(true),
            vec![
                "DCIM/100GOPRO/GL010001.LRV",
                "DCIM/100GOPRO/GX010001.MP4",
                "DCIM/100MEDIA/DJI_0001.LRF",
                "DCIM/100MEDIA/DJI_0001.MP4",
                "PRIVATE/M4ROOT/CLIP/C0001.MP4",
                "PRIVATE/M4ROOT/CLIP/C0001M01.XML",
                "PRIVATE/M4ROOT/SUB/C0001S03.MP4",
            ]
        );
    }

    #[test]
    fn same_name_same_size_needs_same_content() {
        let d = tempfile::tempdir().unwrap();
        let t = tempfile::tempdir().unwrap();
        let len = 200 * 1024;
        let write = |root: &Path, rel: &str, data: &[u8], ts: i64| {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, data).unwrap();
            filetime::set_file_mtime(&p, FileTime::from_unix_time(ts, 0)).unwrap();
        };
        let mine = vec![1u8; len];
        // 另一台同型号机身的同名文件：大小相同，内容不同
        write(d.path(), "DCIM/100MSDCF/DSC00001.ARW", &mine, noon(20));
        write(t.path(), "2026/10/20/DSC00001.ARW", &vec![2u8; len], noon(20));
        // 只有尾部不同
        let mut tail = mine.clone();
        tail[len - 1] = 9;
        write(d.path(), "DCIM/100MSDCF/DSC00002.ARW", &mine, noon(20));
        write(t.path(), "2026/10/20/DSC00002.ARW", &tail, noon(20));
        // 内容相同、mtime 不同（别的工具拷过且没保留 mtime）→ 仍视为已备份
        write(d.path(), "DCIM/100MSDCF/DSC00003.ARW", &mine, noon(20));
        write(t.path(), "2026/10/20/DSC00003.ARW", &mine, noon(21));

        let m = Manifest::open_in_memory().unwrap();
        let input = PlanInput { card_root: d.path(), card_id: "U", card_name: "A7", target_root: t.path(), layout: &FolderTemplate::default(), manifest: &m, include_proxies: false };
        let p = plan_with(&input, &mut no_exif).unwrap();
        let names: Vec<_> = p.tasks.iter().map(|t| t.dst.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec!["DSC00001_1.ARW", "DSC00002_1.ARW"]);
        assert_eq!(p.skipped.len(), 1);
        assert!(p.skipped[0].dst.ends_with("DSC00003.ARW"));
    }

    #[test]
    fn manifest_hit_skips() {
        let d = tempfile::tempdir().unwrap();
        touch(d.path(), "DCIM/100CANON/IMG_0001.CR3", 10, noon(20));
        let t = tempfile::tempdir().unwrap();
        let m = Manifest::open_in_memory().unwrap();
        let input = PlanInput { card_root: d.path(), card_id: "U", card_name: "EOS", target_root: t.path(), layout: &FolderTemplate::default(), manifest: &m, include_proxies: false };
        let p = plan_with(&input, &mut no_exif).unwrap();
        let task = &p.tasks[0];
        // 模拟已拷贝：目标被改名到别处也能通过 manifest 跳过
        let moved = t.path().join("elsewhere.cr3");
        fs::write(&moved, vec![7u8; 10]).unwrap();
        m.record(&crate::manifest::FileRecord { volume_uuid: "U", rel_path: &task.rel_path, size: task.size, mtime: task.mtime, hash: Some(1), dest_path: &moved }).unwrap();
        let p2 = plan_with(&input, &mut no_exif).unwrap();
        assert!(p2.tasks.is_empty());
        assert!(!p2.skipped[0].needs_record);
    }

    #[test]
    fn space_check() {
        let t = tempfile::tempdir().unwrap();
        assert!(check_space(1, t.path()).is_ok());
        let e = check_space(u64::MAX / 2, t.path()).unwrap_err();
        assert!(e.need > 0);
    }
}
