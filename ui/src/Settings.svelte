<script lang="ts">
  import { onMount } from "svelte";
  import {
    api,
    on,
    fmtBytes,
    fmtWhen,
    type Config,
    type TargetStatus,
    type JobRow,
    type AppInfo,
    type FolderLayout,
    type PopoverDensity,
  } from "./lib/api";
  import Switch from "./lib/Switch.svelte";
  import Segmented from "./lib/Segmented.svelte";
  import Icon from "./lib/Icon.svelte";

  type Page = "general" | "backup" | "notifications" | "about";
  const pages: { id: Page; label: string }[] = [
    { id: "general", label: "通用" },
    { id: "backup", label: "备份" },
    { id: "notifications", label: "通知" },
    { id: "about", label: "关于" },
  ];

  let page = $state<Page>("backup");
  let cfg = $state<Config | null>(null);
  let target = $state<TargetStatus | null>(null);
  let jobs = $state<JobRow[]>([]);
  let info = $state<AppInfo | null>(null);
  let error = $state("");

  async function refresh() {
    [cfg, target] = await Promise.all([api.getSettings(), api.targetStatus()]);
    if (page === "about") loadAbout();
  }

  async function loadAbout() {
    [jobs, info] = await Promise.all([api.recentJobs(20), api.appInfo()]);
  }

  onMount(() => {
    refresh();
    const subs = [
      on<Config>("settings-changed", (c) => {
        cfg = c;
        api.targetStatus().then((t) => (target = t));
      }),
      on("window-shown", refresh),
      on<string>("settings-page", (p) => selectPage(p as Page)),
      on("backup-progress", () => {
        // 目标盘插拔会改变连接状态
        api.targetStatus().then((t) => (target = t));
      }),
    ];
    return () => subs.forEach((p) => p.then((u) => u()));
  });

  function selectPage(p: Page) {
    page = p;
    if (p === "about") loadAbout();
  }

  async function update<K extends keyof Config>(key: K, value: Config[K]) {
    if (!cfg) return;
    error = "";
    try {
      cfg = await api.setSettings({ ...cfg, [key]: value });
    } catch (e) {
      error = String(e);
    }
  }

  async function changeTarget() {
    error = "";
    try {
      const c = await api.pickTargetFolder();
      if (c) cfg = c;
      target = await api.targetStatus();
    } catch (e) {
      error = String(e);
    }
  }

  const layoutOptions: { value: FolderLayout; label: string }[] = [
    { value: "date", label: "按日期" },
    { value: "camera", label: "按相机" },
    { value: "card", label: "按卡名" },
  ];
  const densityOptions: { value: PopoverDensity; label: string }[] = [
    { value: "compact", label: "精简" },
    { value: "standard", label: "标准" },
    { value: "detailed", label: "详细" },
  ];

  let layoutExample = $derived(
    cfg?.folder_layout === "camera"
      ? "Canon EOS R5/2026/10/20/IMG_4821.CR3"
      : cfg?.folder_layout === "card"
        ? "EOS_DIGITAL/2026/10/20/IMG_4821.CR3"
        : "2026/10/20/IMG_4821.CR3",
  );

  function statusLabel(j: JobRow): { text: string; tone: string } {
    switch (j.status) {
      case "done":
        return { text: "完成", tone: "tone-ok" };
      case "no_new_files":
        return { text: "无新文件", tone: "tone-ac" };
      case "failed":
        return { text: "失败", tone: "tone-dg" };
      case "running":
        return { text: "进行中", tone: "tone-ac" };
      case "waiting":
        return { text: "等待目标盘", tone: "tone-wn" };
      default:
        return { text: "已中断", tone: "tone-wn" };
    }
  }
</script>

<div class="window">
  <nav class="sidebar" aria-label="设置分区">
    {#each pages as p (p.id)}
      <button class="nav" class:active={page === p.id} aria-current={page === p.id ? "page" : undefined} onclick={() => selectPage(p.id)}>
        {p.label}
      </button>
    {/each}
  </nav>

  <main class="content">
    {#if !cfg}
      <div class="muted">正在载入…</div>
    {:else if page === "backup"}
      <h1 class="title">备份</h1>

      <section>
        <h2>目标</h2>
        <div class="group">
          <div class="row">
            <div class="col">
              <span>备份到</span>
              <span class="mono muted selectable path">{cfg.target_path ?? "尚未选择"}</span>
              {#if target?.configured}
                <span class="small" style="color: {target.connected ? 'var(--ok)' : 'var(--wn)'}">
                  {target.connected
                    ? `已连接${target.free_bytes != null ? ` · ${fmtBytes(target.free_bytes)} 可用` : ""}`
                    : "未连接 · 插卡后会等待它接入"}
                </span>
              {/if}
            </div>
            <button class="btn sm" onclick={changeTarget}>{cfg.target_path ? "更改…" : "选择…"}</button>
          </div>
          <div class="row">
            <div class="col">
              <span>文件夹命名</span>
              <span class="mono muted small">{layoutExample}</span>
            </div>
            <Segmented label="文件夹命名" value={cfg.folder_layout} options={layoutOptions} onchange={(v) => update("folder_layout", v)} />
          </div>
          <div class="row">
            <div class="col">
              <span>备份代理文件</span>
              <span class="small muted">DJI .LRF、GoPro / Insta360 .LRV、Sony SUB 目录里的低码率预览，可由原片重新生成</span>
            </div>
            <Switch label="备份代理文件" checked={cfg.backup_proxies} onchange={(v) => update("backup_proxies", v)} />
          </div>
        </div>
      </section>

      <section>
        <h2>完成后</h2>
        <div class="group">
          <div class="row">
            <div class="col">
              <span>复制后逐个校验</span>
              <span class="small muted">从 SSD 回读，确认每个文件与卡上一致</span>
            </div>
            <Switch label="复制后逐个校验" checked={cfg.verify} onchange={(v) => update("verify", v)} />
          </div>
          <div class="row">
            <div class="col">
              <span>同时回读存储卡</span>
              <span class="small muted">偏执模式：再读一遍卡上的原文件，耗时约翻倍</span>
            </div>
            <Switch label="同时回读存储卡" checked={cfg.paranoid_verify} onchange={(v) => update("paranoid_verify", v)} />
          </div>
          <div class="row">
            <div class="col">
              <span>校验通过后弹出卡</span>
              <span class="small muted">失败时不会弹出</span>
            </div>
            <Switch label="校验通过后弹出卡" checked={cfg.auto_eject} onchange={(v) => update("auto_eject", v)} />
          </div>
        </div>
      </section>

      <section>
        <h2>显示</h2>
        <div class="group">
          <div class="row">
            <span>弹窗信息量</span>
            <Segmented label="弹窗信息量" value={cfg.popover_density} options={densityOptions} onchange={(v) => update("popover_density", v)} />
          </div>
        </div>
      </section>
    {:else if page === "general"}
      <h1 class="title">通用</h1>
      <section>
        <h2>启动</h2>
        <div class="group">
          <div class="row">
            <div class="col">
              <span>登录时打开 Offload</span>
              <span class="small muted">常驻菜单栏，插卡即备份</span>
            </div>
            <Switch label="登录时打开 Offload" checked={cfg.launch_at_login} onchange={(v) => update("launch_at_login", v)} />
          </div>
          <div class="row">
            <span>首次引导</span>
            <button class="btn sm" onclick={() => api.showOnboarding()}>重新查看…</button>
          </div>
        </div>
      </section>
      <section>
        <h2>建议</h2>
        <div class="callout tone-ac">
          <Icon name="info" color="var(--acink)" />
          <div class="body">
            <span class="h">避免「照片」App 抢占存储卡</span>
            <span class="small">打开「图像捕捉」，选中相机卡，把“连接此设备时打开”设为“无”。</span>
          </div>
        </div>
      </section>
    {:else if page === "notifications"}
      <h1 class="title">通知</h1>
      <section>
        <div class="group">
          <div class="row">
            <div class="col">
              <span>开始备份时</span>
              <span class="small muted">正在备份「EOS_DIGITAL」：312 个新文件，48.2 GB</span>
            </div>
            <Switch label="开始备份时通知" checked={cfg.notify_start} onchange={(v) => update("notify_start", v)} />
          </div>
          <div class="row">
            <div class="col">
              <span>完成、出错或等待目标盘时</span>
              <span class="small muted">已备份 312 个文件并全部校验，卡已弹出</span>
            </div>
            <Switch label="完成或出错时通知" checked={cfg.notify_finish} onchange={(v) => update("notify_finish", v)} />
          </div>
        </div>
        <p class="small muted">通知的样式和声音可在“系统设置 › 通知 › Offload”中调整。</p>
      </section>
    {:else if page === "about"}
      <h1 class="title">关于</h1>
      <div class="about">
        <div class="plate tone-ac big"><Icon name="card-up" size={28} /></div>
        <div class="col">
          <span class="h">Offload {info?.version ?? ""}</span>
          <span class="small muted">插入相机卡，自动备份到 SSD。只读源卡，校验通过前绝不弹出。</span>
        </div>
      </div>

      <section>
        <h2>最近备份</h2>
        {#if jobs.length === 0}
          <div class="group"><div class="row muted">还没有备份记录</div></div>
        {:else}
          <div class="group">
            {#each jobs as j (j.id)}
              {@const s = statusLabel(j)}
              <div class="row">
                <div class="col">
                  <span>{j.card_name ?? "存储卡"}</span>
                  <span class="small muted num">
                    {fmtWhen(j.started_at)} · {j.copied} 个新文件{j.skipped ? `，跳过 ${j.skipped} 个` : ""}{j.bytes ? ` · ${fmtBytes(j.bytes)}` : ""}
                  </span>
                  {#if j.error}<span class="small" style="color: var(--dg)">{j.error}</span>{/if}
                </div>
                <span class="chip {s.tone}">{s.text}</span>
              </div>
            {/each}
          </div>
        {/if}
      </section>

      {#if info}
        <section>
          <h2>增量记录</h2>
          <div class="group">
            <div class="row">
              <div class="col">
                <span>已记录 {info.files_recorded} 个文件</span>
                <span class="mono muted small selectable path">{info.manifest_path}</span>
              </div>
            </div>
          </div>
        </section>
      {/if}
      <div>
        <button class="btn sm" onclick={() => api.quit()}>退出 Offload</button>
      </div>
    {/if}

    {#if error}
      <div class="callout tone-dg" role="alert">
        <Icon name="alert-circle" color="var(--dg)" />
        <div class="body"><span class="small">{error}</span></div>
      </div>
    {/if}
  </main>
</div>

<style>
  .window {
    display: flex;
    min-height: 100vh;
    background: var(--s1);
  }
  .sidebar {
    width: 168px;
    flex-shrink: 0;
    padding: 16px 12px;
    display: flex;
    flex-direction: column;
    gap: 4px;
    border-right: 1px solid var(--hair);
  }
  .nav {
    height: 36px;
    display: flex;
    align-items: center;
    padding: 0 12px;
    border: 0;
    border-radius: var(--r-md);
    background: transparent;
    color: var(--mut);
    text-align: left;
  }
  .nav:hover {
    background: var(--s3);
  }
  .nav.active {
    background: var(--acsoft);
    color: var(--acink);
    font-weight: 600;
  }
  .content {
    flex-grow: 1;
    min-width: 0;
    padding: 20px 24px 24px;
    display: flex;
    flex-direction: column;
    gap: 20px;
    overflow-y: auto;
    max-height: 100vh;
  }
  h1 {
    margin: 0;
  }
  section {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  h2 {
    margin: 0;
    font-size: 13px;
    line-height: 18px;
    font-weight: 600;
  }
  .group {
    background: var(--s2);
    border-radius: var(--r-lg);
    display: flex;
    flex-direction: column;
  }
  .row {
    min-height: 48px;
    padding: 8px 12px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }
  .row + .row {
    border-top: 1px solid var(--hair);
  }
  .col {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .path {
    overflow-wrap: anywhere;
  }
  .h {
    font-weight: 600;
  }
  p {
    margin: 0;
  }
  .about {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .big {
    width: 52px;
    height: 52px;
    border-radius: var(--r-lg);
  }
  .chip {
    font-size: 11px;
    line-height: 14px;
    padding: 3px 8px;
    border-radius: 999px;
    font-weight: 600;
    flex-shrink: 0;
  }
</style>
