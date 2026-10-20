//! PhotoSync 核心库：不依赖 Tauri，可单独用命令行和单元测试验证。
//!
//! 主链：VolumeWatcher → CardDetector → JobQueue → Planner → CopyEngine → Manifest → Ejector

pub mod config;
pub mod copier;
pub mod detector;
pub mod job;
pub mod manifest;
pub mod planner;
pub mod platform;
pub mod progress;
pub mod service;

pub use config::{Config, FolderLayout, PopoverDensity};
pub use job::{Command, Job, Notice};
pub use progress::{fmt_bytes, FailReason, JobState, Progress, Snapshot};
pub use service::Service;
