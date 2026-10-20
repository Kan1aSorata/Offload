# Offload

常驻菜单栏的 macOS 小工具：插入含 DCIM 的存储卡后，自动把照片和视频增量备份到指定 SSD，按 `年/月/日` 归档，逐文件 xxh3-128 校验，全部通过后自动弹出卡。

- 轻量：常驻内存约 50 MB 以内（调试构建实测 48 MB），无 Dock 图标
- 无感：插卡后全程无需点击
- 极速：读写流水线 + 8 MB 分块，总耗时约等于 max(读, 写)
- 安全：只读源卡；`F_NOCACHE` 回读目标盘校验；`.offload-part` + 原子重命名；校验通过前绝不弹卡

## 工程结构

```
Offload/
├── crates/core/              # 纯 Rust 库，无 Tauri 依赖
│   ├── platform/mod.rs       # trait VolumeWatcher / Ejector、卷信息、F_NOCACHE
│   ├── platform/macos.rs     # DiskArbitration 监听、diskutil 弹出、IOPMAssertion
│   ├── platform/windows.rs   # 预留
│   ├── detector.rs           # 卡识别：DCIM、非 /、非目标盘；去抖；UUID / 兜底标识
│   ├── planner.rs            # 扫描、EXIF 日期、附属文件归组、跳过/重命名、空间检查
│   ├── copier.rs             # 边拷贝边哈希、回读校验、原子重命名
│   ├── manifest.rs           # SQLite 增量记录与任务历史
│   ├── job.rs                # 任务队列与状态机、系统通知文案
│   ├── progress.rs           # 状态快照、3 秒滑动平均速度、暂停/取消
│   └── service.rs            # watcher → detector → runner 串联
├── crates/cli/               # 调试工具：offload-cli watch | plan | copy | backup | volumes
├── src-tauri/                # 托盘、窗口、通知、命令、10 Hz 事件桥接
├── ui/                       # 菜单栏弹窗、设置窗口、首次引导（Svelte 5）
└── scripts/gen_icons.py      # 生成应用图标与菜单栏模板图标
```

状态机：`Detected → Scanning → Copying → Verifying → Ejecting → Done`；任一步出错进入 `Failed(reason)`；目标盘未连接时进入 `WaitingForTarget`，目标盘挂载后自动继续。

## 开发

需要 Rust（stable）、Node 与 pnpm、Xcode 命令行工具。

```bash
pnpm install
```

```bash
pnpm tauri dev
```

只调界面时可以在浏览器里预览（注入了模拟数据，不连 Rust）：

```bash
pnpm dev
```

然后打开 `http://localhost:1420/?state=copying#/popover`。`state` 可选 `idle`、`scanning`、`copying`、`paused`、`done`、`nonew`、`nospace`、`verify`、`removed`、`waiting`、`notarget`；把 `#/popover` 换成 `#/settings` 或 `#/onboarding` 查看其他窗口，加 `&density=compact|detailed` 查看弹窗信息量。

测试与检查：

```bash
cargo test --workspace
```

```bash
pnpm check
```

打包（生成 `.app` 与 `.dmg`，分发前需 Developer ID 签名并公证；不开启 App Sandbox）：

```bash
pnpm tauri build
```

## 命令行调试工具

```bash
cargo run -p offload-cli -- watch
```

打印每个磁盘事件的 UUID、路径、BSD 名、是否有 DCIM，用于验证扩展坞插拔、睡眠唤醒、多卡同插。

```bash
cargo run -p offload-cli -- plan /Volumes/EOS_DIGITAL "/Volumes/Photos SSD/Cards" -v
```

只打印计划：每组文件的日期来源（EXIF / mtime）、目标路径、跳过与重命名。加 `--proxies` 同时计入代理文件。

```bash
cargo run --release -p offload-cli -- copy <源文件> <目标文件>
```

单文件拷贝并回读校验，显示吞吐；Ctrl-C 只会留下被清理的 `.offload-part`。

```bash
cargo run -p offload-cli -- backup /Volumes/EOS_DIGITAL "/Volumes/Photos SSD/Cards" --eject
```

对一张卡执行完整流程（默认使用应用的 manifest，可用 `--manifest` 指定测试库）。

## 归档与增量规则

- 目标路径 `<目标根>/YYYY/MM/DD/<原文件名>`；设置里可改为「按相机」`<型号>/YYYY/MM/DD` 或「按卡名」`<卡名>/YYYY/MM/DD`
- 照片取 EXIF DateTimeOriginal（含 CR3/HEIF），视频与无 EXIF 的文件取 mtime
- 附属文件（XMP、THM、XML、LRF）跟随同名主文件，包括 Sony `C0001.MP4` ↔ `C0001M01.XML`
- manifest 有记录且目标仍在 → 跳过；目标存在、大小相同且头尾各 64 KB 一致 → 跳过并补记；否则整组加后缀 `_1`、`_2`（两台同型号机身拍出的同名、同大小无压缩 RAW 不会被误判为已备份）
- 扫描 DCIM 下全部文件，以及其他目录中的已知媒体（如 `PRIVATE/M4ROOT/CLIP`）；忽略 `._*`、`.Spotlight-V100`、`.Trashes`、`.fseventsd`
- 不备份卡索引与缩略图：Canon `DCIM/CANONMSC`、Sony `THMBNL`、`MEDIAPRO.XML`、`CUEUP.XML`
- 代理文件（DJI `.LRF`、GoPro / Insta360 `.LRV`、Sony `PRIVATE/M4ROOT/SUB`）默认不备份，可在设置中打开
- 多张卡依次排队；目标盘未连接时所有卡一起等待，接入后按插入顺序继续；「重试」会重新排队所有失败的卡

## 数据位置

| 内容 | 路径 |
| --- | --- |
| 配置 | `~/Library/Application Support/app.offload.desktop/settings.json` |
| 增量记录 | `~/Library/Application Support/Offload/manifest.db` |

## 权限与已知事项

- 首次读取存储卡时系统会请求「可移除宗卷」权限（`NSRemovableVolumesUsageDescription`）；开发阶段该权限归属终端 App，属正常现象
- 建议在「图像捕捉」中把相机卡的“连接此设备时打开”设为“无”，避免「照片」App 抢卡
- 弹出暂用 `diskutil eject`，被占用时按 1、2、4 秒退避重试，并在通知中写明占用进程
- Windows 版只预留了 `VolumeWatcher` / `Ejector` 接口，尚未实现
