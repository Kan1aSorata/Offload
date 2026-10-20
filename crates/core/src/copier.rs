//! 边拷贝边哈希：源文件只读一次，同一块缓冲区先喂给 xxh3 再写入目标盘。
//!
//! 读线程和写线程通过一个缓冲池组成流水线，总耗时约等于 max(读, 写)。

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::thread;

use crossbeam_channel::bounded;
use xxhash_rust::xxh3::Xxh3;

use crate::platform::set_nocache;
use crate::progress::{Cancelled, FileStage, Progress};

/// 8 MB，对 SD 顺序读友好
pub const CHUNK: usize = 8 << 20;
/// 3 块缓冲 = 24 MB 常驻
pub const DEPTH: usize = 3;

pub const PART_SUFFIX: &str = ".offload-part";
/// 改名前（PhotoSync）的临时文件后缀，只用于清理残留
pub const LEGACY_PART_SUFFIX: &str = ".photosync-part";

#[derive(Debug, thiserror::Error)]
pub enum CopyError {
    #[error("读取源文件失败：{0}")]
    Source(#[source] io::Error),
    #[error("写入目标失败：{0}")]
    Dest(#[source] io::Error),
    #[error("校验不一致：{0}")]
    HashMismatch(PathBuf),
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CopyOptions {
    /// 写入后从目标盘回读校验
    pub verify: bool,
    /// 额外回读源卡（偏执模式）
    pub paranoid: bool,
}

/// `dst` 对应的临时文件：`.<名>.offload-part`
pub fn part_path(dst: &Path) -> PathBuf {
    part_path_with(dst, PART_SUFFIX)
}

pub fn part_path_with(dst: &Path, suffix: &str) -> PathBuf {
    let name = dst.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    dst.with_file_name(format!(".{name}{suffix}"))
}

/// 出错或取消时删掉临时文件，避免留下看似完整的残缺文件
struct PartGuard<'a>(&'a Path, bool);

impl Drop for PartGuard<'_> {
    fn drop(&mut self) {
        if !self.1 {
            let _ = fs::remove_file(self.0);
        }
    }
}

/// 拷贝一个文件并校验，返回源文件的 xxh3-128。
///
/// 只有校验通过后才会 rename 为正式文件名：存在正式文件名 = 已校验完成。
pub fn copy_verified(src: &Path, dst: &Path, prog: &Progress, opts: CopyOptions) -> Result<u128, CopyError> {
    if let Some(dir) = dst.parent() {
        fs::create_dir_all(dir).map_err(CopyError::Dest)?;
    }
    prog.set_stage(FileStage::Copying); // 重试时从头计
    let part = part_path(dst);
    let src_meta = fs::metadata(src).map_err(CopyError::Source)?;
    let mut input = File::open(src).map_err(CopyError::Source)?;
    let _ = set_nocache(&input);
    let mut out = File::create(&part).map_err(CopyError::Dest)?;
    let mut guard = PartGuard(&part, false);
    let _ = set_nocache(&out);

    let (full_tx, full_rx) = bounded::<Vec<u8>>(DEPTH);
    let (empty_tx, empty_rx) = bounded::<Vec<u8>>(DEPTH);
    for _ in 0..DEPTH {
        empty_tx.send(vec![0u8; CHUNK]).expect("buffer pool");
    }

    let reader = thread::Builder::new()
        .name("copy-read".into())
        .spawn(move || -> io::Result<()> {
            for mut buf in empty_rx {
                buf.resize(CHUNK, 0);
                let n = read_full(&mut input, &mut buf)?; // 循环读到填满或 EOF
                if n == 0 {
                    break;
                }
                buf.truncate(n);
                if full_tx.send(buf).is_err() {
                    break; // 写端已退出
                }
            }
            Ok(())
        })
        .map_err(CopyError::Source)?;

    let mut h = Xxh3::new();
    let mut written: u64 = 0;
    let write_result: Result<(), CopyError> = (|| {
        for buf in &full_rx {
            prog.checkpoint()?;
            h.update(&buf);
            out.write_all(&buf).map_err(CopyError::Dest)?;
            written += buf.len() as u64;
            prog.add(buf.len());
            let _ = empty_tx.send(buf); // 缓冲区回收复用
        }
        Ok(())
    })();
    drop(full_rx);
    drop(empty_tx);
    let read_result = reader.join().unwrap_or_else(|_| Err(io::Error::other("读线程崩溃")));
    if let Err(e) = write_result {
        prog.sub(written);
        return Err(e);
    }
    if let Err(e) = read_result {
        prog.sub(written);
        return Err(CopyError::Source(e));
    }
    if written != src_meta.len() {
        prog.sub(written);
        return Err(CopyError::Source(io::Error::new(io::ErrorKind::UnexpectedEof, "源文件长度变化")));
    }

    prog.set_stage(FileStage::Syncing);
    out.sync_all().map_err(CopyError::Dest)?; // macOS 上 = F_FULLFSYNC
    drop(out);
    let src_hash = h.digest128();

    if opts.verify {
        prog.set_stage(FileStage::Verifying);
        let dst_hash = hash_file_nocache(&part, Some(prog)).map_err(CopyError::Dest)?; // 回读的是磁盘，不是页缓存
        if src_hash != dst_hash {
            prog.sub(written);
            return Err(CopyError::HashMismatch(src.to_path_buf()));
        }
    }
    if opts.paranoid {
        prog.set_stage(FileStage::VerifyingSource);
        let again = hash_file_nocache(src, Some(prog)).map_err(CopyError::Source)?;
        if again != src_hash {
            prog.sub(written);
            return Err(CopyError::HashMismatch(src.to_path_buf()));
        }
    }

    if let Ok(mtime) = src_meta.modified() {
        let _ = filetime::set_file_mtime(&part, filetime::FileTime::from_system_time(mtime));
    }
    fs::rename(&part, dst).map_err(CopyError::Dest)?; // 原子重命名
    guard.1 = true;
    Ok(src_hash)
}

/// 以 F_NOCACHE 读取整个文件并计算 xxh3-128
pub fn hash_file_nocache(path: &Path, prog: Option<&Progress>) -> io::Result<u128> {
    let mut f = File::open(path)?;
    let _ = set_nocache(&f);
    let mut h = Xxh3::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        if let Some(p) = prog {
            p.checkpoint().map_err(|_| io::Error::new(io::ErrorKind::Interrupted, "已取消"))?;
        }
        let n = read_full(&mut f, &mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        if let Some(p) = prog {
            p.add_stage(n);
        }
    }
    Ok(h.digest128())
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// 把 128 位哈希编码为 16 字节大端，用于写入 manifest
pub fn hash_bytes(h: u128) -> [u8; 16] {
    h.to_be_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_random(path: &Path, len: usize) {
        let mut x: u64 = 0x9E3779B97F4A7C15;
        let data: Vec<u8> = (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        fs::write(path, data).unwrap();
    }

    #[test]
    fn copies_and_verifies_multi_chunk() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("IMG_0001.CR3");
        let len = CHUNK * 2 + 12345;
        write_random(&src, len);
        let dst = d.path().join("out/2026/10/20/IMG_0001.CR3");
        let p = Progress::new();
        let h = copy_verified(&src, &dst, &p, CopyOptions { verify: true, paranoid: true }).unwrap();
        assert_eq!(fs::read(&src).unwrap(), fs::read(&dst).unwrap());
        assert_eq!(h, hash_file_nocache(&dst, None).unwrap());
        assert!(!part_path(&dst).exists());
        let s = p.snapshot();
        assert_eq!(s.bytes_done, len as u64);
        // 校验回读不计入总进度，但推进文件内进度
        assert_eq!(s.file_stage, Some(FileStage::VerifyingSource));
        assert_eq!(s.file_bytes_done, len as u64);
        // mtime 被还原
        let a = fs::metadata(&src).unwrap().modified().unwrap();
        let b = fs::metadata(&dst).unwrap().modified().unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn empty_file() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("a.xmp");
        fs::write(&src, b"").unwrap();
        let dst = d.path().join("b/a.xmp");
        copy_verified(&src, &dst, &Progress::new(), CopyOptions { verify: true, paranoid: false }).unwrap();
        assert!(dst.exists());
    }

    #[test]
    fn cancel_leaves_no_files() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("v.mp4");
        write_random(&src, CHUNK + 1);
        let dst = d.path().join("o/v.mp4");
        let p = Progress::new();
        p.cancel();
        let r = copy_verified(&src, &dst, &p, CopyOptions::default());
        assert!(matches!(r, Err(CopyError::Cancelled(_))));
        assert!(!dst.exists());
        assert!(!part_path(&dst).exists());
        assert_eq!(p.snapshot().bytes_done, 0);
    }

    #[test]
    fn missing_source() {
        let d = tempfile::tempdir().unwrap();
        let r = copy_verified(&d.path().join("nope"), &d.path().join("x"), &Progress::new(), CopyOptions::default());
        assert!(matches!(r, Err(CopyError::Source(_))));
    }
}
