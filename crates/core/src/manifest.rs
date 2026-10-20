//! 增量记录（SQLite）：增量判断、审计、断点续传。
//!
//! 默认位于 `~/Library/Application Support/Offload/manifest.db`。

use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::progress::unix_now;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS files (
  volume_uuid  TEXT    NOT NULL,
  rel_path     TEXT    NOT NULL,   -- 相对卡根目录
  size         INTEGER NOT NULL,
  mtime        INTEGER NOT NULL,
  xxh3_128     BLOB,               -- 规则 2 补记时可为空
  dest_path    TEXT    NOT NULL,
  verified_at  INTEGER NOT NULL,
  PRIMARY KEY (volume_uuid, rel_path, size, mtime)
);
CREATE TABLE IF NOT EXISTS jobs (
  id INTEGER PRIMARY KEY, volume_uuid TEXT, started_at INTEGER, finished_at INTEGER,
  status TEXT, copied INTEGER, skipped INTEGER, bytes INTEGER, error TEXT,
  card_name TEXT, dest_dir TEXT
);
";

pub struct Manifest {
    conn: Connection,
}

#[derive(Debug, Clone)]
pub struct FileRecord<'a> {
    pub volume_uuid: &'a str,
    pub rel_path: &'a str,
    pub size: u64,
    pub mtime: i64,
    pub hash: Option<u128>,
    pub dest_path: &'a Path,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobRow {
    pub id: i64,
    pub volume_uuid: String,
    pub card_name: Option<String>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub status: String,
    pub copied: u32,
    pub skipped: u32,
    pub bytes: u64,
    pub error: Option<String>,
    pub dest_dir: Option<String>,
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

pub fn default_path() -> PathBuf {
    home().join("Library/Application Support/Offload/manifest.db")
}

/// 改名前（PhotoSync）的数据目录
const LEGACY_DIR: &str = "Library/Application Support/PhotoSync";

/// 改名后首次启动：把旧目录里的增量记录搬到 `path`，否则下次插卡会把已备份的文件重拷一遍。
/// 打开数据库之前调用。
pub fn migrate_legacy(path: &Path) {
    match migrate_from(&home().join(LEGACY_DIR), path) {
        Ok(true) => tracing::info!(to = %path.display(), "已迁移 PhotoSync 的增量记录"),
        Ok(false) => {}
        Err(e) => tracing::warn!(error = %e, "迁移 PhotoSync 增量记录失败"),
    }
}

/// 一条文件记录和任务记录都没有：可以放心被旧库覆盖
fn is_empty(path: &Path) -> bool {
    Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .and_then(|c| c.query_row("SELECT (SELECT count(*) FROM files) + (SELECT count(*) FROM jobs)", [], |r| r.get::<_, i64>(0)))
        .map(|n| n == 0)
        .unwrap_or(false)
}

fn migrate_from(legacy_dir: &Path, path: &Path) -> std::io::Result<bool> {
    let old = legacy_dir.join("manifest.db");
    if !old.exists() || (path.exists() && !is_empty(path)) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // WAL 里可能还有未合并的记录，三个文件一起搬；主库最后搬，中途失败下次还会重试。
    // 新位置上已有的空库（迁移前先启动过一次）连同它的 -wal/-shm 一起被覆盖或删掉。
    for suffix in ["-wal", "-shm"] {
        let from = legacy_dir.join(format!("manifest.db{suffix}"));
        let to = path.with_file_name(format!("manifest.db{suffix}"));
        if from.exists() {
            std::fs::rename(&from, &to)?;
        } else if to.exists() {
            std::fs::remove_file(&to)?;
        }
    }
    std::fs::rename(&old, path)?;
    let _ = std::fs::remove_dir(legacy_dir); // 只在目录已空时成功
    Ok(true)
}

impl Manifest {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        // 旧库补列
        for col in ["card_name TEXT", "dest_dir TEXT"] {
            let _ = conn.execute(&format!("ALTER TABLE jobs ADD COLUMN {col}"), []);
        }
        Ok(Self { conn })
    }

    /// 规则 1：该卡该文件已记录（大小与 mtime 一致）时返回当时的目标路径
    pub fn lookup(&self, volume_uuid: &str, rel_path: &str, size: u64, mtime: i64) -> Result<Option<PathBuf>> {
        Ok(self
            .conn
            .query_row(
                "SELECT dest_path FROM files WHERE volume_uuid=?1 AND rel_path=?2 AND size=?3 AND mtime=?4",
                params![volume_uuid, rel_path, size as i64, mtime],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .map(PathBuf::from))
    }

    pub fn record(&self, rec: &FileRecord) -> Result<()> {
        let hash = rec.hash.map(|h| h.to_be_bytes().to_vec());
        self.conn.execute(
            "INSERT INTO files (volume_uuid, rel_path, size, mtime, xxh3_128, dest_path, verified_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(volume_uuid, rel_path, size, mtime) DO UPDATE SET
               xxh3_128 = COALESCE(excluded.xxh3_128, files.xxh3_128),
               dest_path = excluded.dest_path, verified_at = excluded.verified_at",
            params![
                rec.volume_uuid,
                rec.rel_path,
                rec.size as i64,
                rec.mtime,
                hash,
                rec.dest_path.to_string_lossy(),
                unix_now()
            ],
        )?;
        Ok(())
    }

    pub fn job_start(&self, volume_uuid: &str, card_name: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO jobs (volume_uuid, started_at, status, copied, skipped, bytes, card_name)
             VALUES (?1, ?2, 'running', 0, 0, 0, ?3)",
            params![volume_uuid, unix_now(), card_name],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn job_finish(
        &self,
        id: i64,
        status: &str,
        copied: u32,
        skipped: u32,
        bytes: u64,
        error: Option<&str>,
        dest_dir: Option<&Path>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE jobs SET finished_at=?2, status=?3, copied=?4, skipped=?5, bytes=?6, error=?7, dest_dir=?8 WHERE id=?1",
            params![
                id,
                unix_now(),
                status,
                copied,
                skipped,
                bytes as i64,
                error,
                dest_dir.map(|p| p.to_string_lossy().into_owned())
            ],
        )?;
        Ok(())
    }

    pub fn recent_jobs(&self, limit: u32) -> Result<Vec<JobRow>> {
        let mut st = self.conn.prepare(
            "SELECT id, volume_uuid, card_name, started_at, finished_at, status, copied, skipped, bytes, error, dest_dir
             FROM jobs ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = st
            .query_map([limit], |r| {
                Ok(JobRow {
                    id: r.get(0)?,
                    volume_uuid: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    card_name: r.get(2)?,
                    started_at: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                    finished_at: r.get(4)?,
                    status: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    copied: r.get::<_, Option<u32>>(6)?.unwrap_or(0),
                    skipped: r.get::<_, Option<u32>>(7)?.unwrap_or(0),
                    bytes: r.get::<_, Option<i64>>(8)?.unwrap_or(0) as u64,
                    error: r.get(9)?,
                    dest_dir: r.get(10)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 被异常中断（进程被杀）的任务标记为 interrupted
    pub fn close_dangling_jobs(&self) -> Result<()> {
        self.conn.execute(
            "UPDATE jobs SET status='interrupted', finished_at=started_at WHERE status='running'",
            [],
        )?;
        Ok(())
    }

    pub fn file_count(&self) -> Result<u64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get::<_, i64>(0))? as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_legacy_manifest() {
        let d = tempfile::tempdir().unwrap();
        let legacy = d.path().join("PhotoSync");
        let path = d.path().join("Offload/manifest.db");
        {
            let m = Manifest::open(&legacy.join("manifest.db")).unwrap();
            m.job_start("CARD", "EOS").unwrap();
        }
        // 迁移前新位置已被建了一个空库
        drop(Manifest::open(&path).unwrap());
        assert!(migrate_from(&legacy, &path).unwrap());
        assert!(!legacy.exists());
        let m = Manifest::open(&path).unwrap();
        assert_eq!(m.recent_jobs(10).unwrap().len(), 1);
        // 已迁移过：不再动；新库有数据时也不会被旧库覆盖
        assert!(!migrate_from(&legacy, &path).unwrap());
        let other = d.path().join("Other");
        drop(Manifest::open(&other.join("manifest.db")).unwrap());
        assert!(!migrate_from(&other, &path).unwrap());
    }

    #[test]
    fn record_and_lookup() {
        let m = Manifest::open_in_memory().unwrap();
        let dest = PathBuf::from("/t/2026/10/20/IMG_1.CR3");
        assert!(m.lookup("U", "DCIM/100/IMG_1.CR3", 10, 5).unwrap().is_none());
        m.record(&FileRecord {
            volume_uuid: "U",
            rel_path: "DCIM/100/IMG_1.CR3",
            size: 10,
            mtime: 5,
            hash: Some(42),
            dest_path: &dest,
        })
        .unwrap();
        assert_eq!(m.lookup("U", "DCIM/100/IMG_1.CR3", 10, 5).unwrap(), Some(dest.clone()));
        // mtime 不同 → 视为新文件
        assert!(m.lookup("U", "DCIM/100/IMG_1.CR3", 10, 6).unwrap().is_none());
        // 补记（hash 为空）不会抹掉已有哈希
        m.record(&FileRecord { volume_uuid: "U", rel_path: "DCIM/100/IMG_1.CR3", size: 10, mtime: 5, hash: None, dest_path: &dest })
            .unwrap();
        let h: Option<Vec<u8>> = m.conn.query_row("SELECT xxh3_128 FROM files", [], |r| r.get(0)).unwrap();
        assert_eq!(h.unwrap().len(), 16);
    }

    #[test]
    fn jobs_roundtrip() {
        let m = Manifest::open_in_memory().unwrap();
        let id = m.job_start("U", "EOS_R5").unwrap();
        m.job_finish(id, "done", 3, 1, 999, None, Some(Path::new("/t"))).unwrap();
        let j = &m.recent_jobs(5).unwrap()[0];
        assert_eq!(j.status, "done");
        assert_eq!(j.copied, 3);
        assert_eq!(j.card_name.as_deref(), Some("EOS_R5"));
    }
}
