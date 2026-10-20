<script lang="ts">
  import { onMount } from "svelte";
  import {
    api,
    on,
    fmtBytes,
    fmtSpeed,
    fmtDuration,
    fmtWhen,
    shortPath,
    percent,
    type Snapshot,
    type Config,
    type TargetStatus,
    type JobRow,
  } from "./lib/api";
  import Icon from "./lib/Icon.svelte";
  import Ring from "./lib/Ring.svelte";

  let snap = $state<Snapshot | null>(null);
  let cfg = $state<Config | null>(null);
  let target = $state<TargetStatus | null>(null);
  let lastJob = $state<JobRow | null>(null);
  let panel: HTMLDivElement;

  async function refresh() {
    const [s, c, t, jobs] = await Promise.all([api.getState(), api.getSettings(), api.targetStatus(), api.recentJobs(1)]);
    snap = s;
    cfg = c;
    target = t;
    lastJob = jobs[0] ?? null;
  }

  onMount(() => {
    refresh();
    let prevState = "";
    const subs = [
      on<Snapshot>("backup-progress", (s) => {
        snap = s;
        // 状态切换时刷新目标盘状态和最近记录
        if (s.state !== prevState) {
          prevState = s.state;
          api.targetStatus().then((t) => (target = t));
          if (s.state === "idle") api.recentJobs(1).then((j) => (lastJob = j[0] ?? null));
        }
      }),
      on<Config>("settings-changed", (c) => {
        cfg = c;
        api.targetStatus().then((t) => (target = t));
      }),
      on("popover-shown", refresh),
    ];
    // 窗口高度跟随内容
    const ro = new ResizeObserver(() => api.fitPopover(Math.ceil(panel.getBoundingClientRect().height)));
    ro.observe(panel);
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") api.hidePopover();
    };
    window.addEventListener("keydown", onKey);
    return () => {
      ro.disconnect();
      window.removeEventListener("keydown", onKey);
      subs.forEach((p) => p.then((u) => u()));
    };
  });

  let phase = $derived(snap?.state ?? "idle");
  let density = $derived(cfg?.popover_density ?? "standard");
  let pct = $derived(snap ? percent(snap.bytes_done, snap.bytes_total) : 0);
  let speed = $derived(fmtSpeed(snap?.speed_bps ?? 0));
  // 当前文件已写完、正在刷盘或回读校验：总进度此时不动，单独显示
  let checking = $derived(phase === "copying" && !!snap?.file_stage && snap.file_stage !== "copying");
  let filePct = $derived(snap ? percent(snap.file_bytes_done, snap.file_bytes_total) : 0);
  let stageLabel = $derived.by(() => {
    switch (snap?.file_stage) {
      case "syncing":
        return "正在写入磁盘";
      case "verifying":
        return "正在回读校验";
      case "verifying_source":
        return "正在回读源卡";
      default:
        return "正在复制";
    }
  });
  // 小文件一闪而过，只给大文件显示单文件进度条
  let showFileBar = $derived(phase === "copying" && (snap?.file_bytes_total ?? 0) >= 100e6);
  let card = $derived(snap?.card_name ?? "存储卡");
  let cap = $derived(snap && snap.card_capacity > 0 ? ` · ${fmtBytes(snap.card_capacity).replace(".0 ", " ")}` : "");
  let targetName = $derived(snap?.target_name ?? target?.name ?? "目标盘");

  let subtitle = $derived.by(() => {
    if (!snap) return "";
    if (snap.paused) return "已暂停 · Paused";
    switch (phase) {
      case "detected":
      case "scanning":
        return "正在扫描 · Scanning";
      case "copying":
        return checking ? "正在校验 · Verifying" : "正在备份 · Copying";
      case "verifying":
        return "正在校验 · Verifying";
      case "ejecting":
        return "正在弹出 · Ejecting";
      default:
        return "";
    }
  });

  let failTitle = $derived.by(() => {
    const f = snap?.fail;
    if (!f) return snap?.last_error ?? "备份已停止";
    switch (f.kind) {
      case "no_space":
        return `目标盘空间不足（还需 ${fmtBytes(f.need_bytes)}），卡未弹出`;
      case "no_target":
        return "尚未设置备份目标，卡未弹出";
      case "card_removed":
        return "存储卡在备份中被拔出";
      case "target_lost":
        return "目标盘已断开，卡未弹出";
      case "verify_failed":
        return f.count === 1
          ? `${f.first} 校验未通过，卡未弹出`
          : `${f.first} 等 ${f.count} 个文件校验未通过，卡未弹出`;
      case "io":
        return `读写出错，卡未弹出`;
    }
  });

  let failSub = $derived.by(() => {
    const f = snap?.fail;
    if (!f) return "";
    switch (f.kind) {
      case "no_space":
        return `Not enough space, ${fmtBytes(f.need_bytes)} more needed. Card not ejected.`;
      case "no_target":
        return "Choose where to back up first.";
      case "card_removed":
        return "Reinsert the card to resume. Finished files are kept.";
      case "target_lost":
        return "Reconnect the SSD to resume.";
      case "verify_failed":
        return "Retry to copy them again. Originals are still on the card.";
      case "io":
        return f.message;
    }
  });

  async function chooseOtherTarget() {
    const c = await api.pickTargetFolder();
    if (c && phase === "failed") await api.retry();
  }

  function finishedTime(): string {
    return fmtWhen(snap?.finished_at);
  }
</script>

<div class="panel" bind:this={panel}>
  {#if !snap}
    <div class="header">
      <div class="plate tone-ac"><Icon name="card-up" /></div>
      <div class="grow"><div class="h">PhotoSync</div></div>
    </div>
  {:else if phase === "idle"}
    <!-- 空闲：等待插卡 -->
    <div class="header">
      <div class="plate tone-ac"><Icon name="card-up" /></div>
      <div class="grow">
        <div class="h">PhotoSync</div>
        <div class="caption muted">等待插卡 · Idle</div>
      </div>
      <button class="icon-btn" aria-label="设置" onclick={() => api.openSettings()}><Icon name="gear" /></button>
    </div>

    {#if target && !target.configured}
      <div class="callout tone-wn">
        <Icon name="alert-circle" color="var(--wn)" />
        <div class="body">
          <div class="h">尚未设置备份目标</div>
          <div class="small">Choose an SSD to back up to.</div>
        </div>
      </div>
      <button class="btn primary" onclick={() => api.pickTargetFolder()}>选择备份目标…</button>
    {:else}
      <div class="empty">
        <div class="h">插入相机卡，自动开始备份</div>
        <div class="small muted">复制后逐个校验，完成后自动弹出</div>
      </div>

      <div class="card">
        <div class="kv">
          <span>备份到</span>
          <span class="status">
            <span class="dot" class:off={!target?.connected}></span>{targetName}
          </span>
        </div>
        {#if target?.connected && target.free_bytes != null}
          <div class="kv"><span>可用空间</span><span class="num">{fmtBytes(target.free_bytes)}</span></div>
        {:else if target && !target.connected}
          <div class="kv"><span>状态</span><span style="color: var(--wn)">未连接</span></div>
        {/if}
        {#if density !== "compact" && lastJob}
          <div class="divider"></div>
          <div class="kv">
            <span>上次备份</span>
            <span class="num">{lastJob.card_name ?? "存储卡"} · {fmtWhen(lastJob.finished_at ?? lastJob.started_at)}</span>
          </div>
          {#if density === "detailed"}
            <div class="kv">
              <span>结果</span>
              <span class="num">
                {lastJob.status === "done" ? `${lastJob.copied} 个文件，${fmtBytes(lastJob.bytes)}` : lastJob.status === "no_new_files" ? "没有新文件" : (lastJob.error ?? "未完成")}
              </span>
            </div>
          {/if}
        {/if}
      </div>

      <div class="actions">
        <button class="btn grow" onclick={() => api.openBackupDir()}>打开备份目录</button>
        <button class="btn text" onclick={() => api.openSettings()}>设置…</button>
      </div>
    {/if}
    <div class="footer caption muted">插卡后全程无需操作 · Just insert a card</div>
  {:else if phase === "detected" || phase === "scanning"}
    <div class="header">
      <div class="plate tone-ac"><Icon name="card-scan" /></div>
      <div class="grow">
        <div class="h">{card}{cap}</div>
        <div class="caption muted">{subtitle}</div>
      </div>
    </div>
    <div class="hero">
      <Ring indeterminate label="正在扫描" />
      <div class="hero-text">
        <div class="h">正在读取文件列表…</div>
        <div class="small muted">读取拍摄日期、比对已备份的文件</div>
      </div>
    </div>
    <div class="footer caption muted">完成前请勿拔出卡 · Keep the card in until done</div>
  {:else if phase === "copying" || phase === "verifying" || phase === "ejecting"}
    <!-- 备份中（设计稿：弹窗 · 备份中） -->
    <div class="header">
      <div class="plate tone-ac"><Icon name="card-up" /></div>
      <div class="grow">
        <div class="h">{card}{cap}</div>
        <div class="caption muted">{subtitle}</div>
      </div>
      {#if phase === "copying"}
        {#if snap.paused}
          <button class="icon-btn" aria-label="继续备份" onclick={() => api.resume()}><Icon name="play" /></button>
        {:else}
          <button class="icon-btn" aria-label="暂停备份" onclick={() => api.pause()}><Icon name="pause" /></button>
        {/if}
      {/if}
    </div>

    <div class="hero">
      <Ring {pct} indeterminate={phase === "ejecting"} label="备份进度" />
      <div class="hero-text">
        {#if checking}
          <div class="h">{snap.file_stage === "syncing" ? "正在写入磁盘…" : `正在校验 ${filePct}%`}</div>
        {:else if phase === "copying"}
          <div class="num speed">
            <span class="display">{snap.paused ? "0" : speed.value}</span>
            <span class="muted">{speed.unit}</span>
          </div>
        {:else}
          <div class="h">{phase === "verifying" ? "正在核对文件…" : "正在弹出存储卡…"}</div>
        {/if}
        <div class="small muted num">{fmtBytes(snap.bytes_done)} / {fmtBytes(snap.bytes_total)}</div>
        {#if phase === "copying" && !snap.paused && snap.eta_secs != null}
          <div class="small muted">约剩 {fmtDuration(snap.eta_secs)}</div>
        {:else if snap.paused}
          <div class="small muted">已暂停，随时可继续</div>
        {/if}
      </div>
    </div>

    {#if density !== "compact"}
      <div class="card">
        {#if snap.current_file && phase === "copying"}
          <div class="current">
            <div class="caption muted">{stageLabel}</div>
            <div class="mono ellipsis">{snap.current_file}</div>
            {#if showFileBar}
              <div
                class="file-bar"
                class:indeterminate={snap.file_stage === "syncing"}
                role="progressbar"
                aria-label="{stageLabel}进度"
                aria-valuemin="0"
                aria-valuemax="100"
                aria-valuenow={snap.file_stage === "syncing" ? undefined : filePct}
              >
                <div style:width="{snap.file_stage === 'syncing' ? 30 : filePct}%"></div>
              </div>
            {/if}
          </div>
          <div class="divider"></div>
        {/if}
        <div class="kv"><span>已复制</span><span class="num">{snap.files_done} / {snap.files_total} 个文件</span></div>
        <div class="kv"><span>备份到</span><span>{targetName}</span></div>
        <div class="kv"><span>校验</span><span>{cfg?.verify ? (cfg.paranoid_verify ? "回读目标盘与源卡" : "复制后逐个校验") : "仅写入，不回读"}</span></div>
        {#if density === "detailed"}
          <div class="kv"><span>已跳过</span><span class="num">{snap.files_skipped} 个已备份文件</span></div>
          <div class="kv"><span>用时</span><span class="num">{fmtDuration(snap.elapsed_secs)}</span></div>
          {#if snap.queued > 0}
            <div class="kv"><span>排队中</span><span class="num">{snap.queued} 张卡</span></div>
          {/if}
        {/if}
      </div>
    {/if}

    <div class="actions">
      {#if phase === "copying"}
        {#if snap.paused}
          <button class="btn primary grow" onclick={() => api.resume()}>继续</button>
        {:else}
          <button class="btn grow" onclick={() => api.pause()}>暂停</button>
        {/if}
      {/if}
      <button class="btn text" class:grow={phase !== "copying"} onclick={() => api.openSettings()}>设置…</button>
    </div>
    <div class="footer caption muted">完成前请勿拔出卡 · Keep the card in until done</div>
  {:else if phase === "done"}
    <!-- 已完成（设计稿：弹窗 · 已完成） -->
    {@const noNew = snap.files_total === 0}
    <div class="header">
      <div class="plate tone-ok"><Icon name="card-check" /></div>
      <div class="grow">
        <div class="h">{card}{cap}</div>
        <div class="caption" style="color: var(--ok)">已完成 · Done</div>
      </div>
    </div>

    <div class="callout tone-ok">
      <Icon name="check-circle" color="var(--ok)" />
      <div class="body">
        {#if noNew}
          <div class="h">没有新文件{snap.ejected ? "，卡已弹出" : ""}</div>
          <div class="small">{snap.files_skipped} 个文件此前已备份。Nothing new on this card.</div>
        {:else if snap.ejected}
          <div class="h">已备份 {snap.files_done} 个文件并全部校验，卡已弹出</div>
          <div class="small">{snap.files_done} files verified. Card ejected, safe to remove.</div>
        {:else}
          <div class="h">已备份 {snap.files_done} 个文件并全部校验</div>
          <div class="small">{snap.files_done} files verified. You can remove the card.</div>
        {/if}
      </div>
    </div>

    {#if snap.eject_error}
      <div class="callout tone-wn">
        <Icon name="alert-circle" color="var(--wn)" />
        <div class="body">
          <div class="h">卡未能弹出</div>
          <div class="small">{snap.eject_error}。文件已安全备份，可在访达中手动推出。</div>
        </div>
      </div>
    {/if}

    {#if density !== "compact" && !noNew}
      <div class="card">
        <div class="kv"><span>文件</span><span class="num">{snap.files_done} 个</span></div>
        <div class="kv"><span>容量</span><span class="num">{fmtBytes(snap.bytes_done)}</span></div>
        <div class="kv"><span>用时</span><span class="num">{fmtDuration(snap.elapsed_secs)}</span></div>
        <div class="kv"><span>平均速度</span><span class="num">{fmtSpeed(snap.avg_speed_bps).value} {fmtSpeed(snap.avg_speed_bps).unit}</span></div>
        <div class="kv">
          <span>校验</span>
          {#if cfg?.verify === false}
            <span>已写入（未回读）</span>
          {:else}
            <span class="pass"><Icon name="check" size={16} />全部通过</span>
          {/if}
        </div>
        {#if density === "detailed" && snap.files_skipped > 0}
          <div class="kv"><span>已跳过</span><span class="num">{snap.files_skipped} 个</span></div>
        {/if}
        <div class="divider"></div>
        <div class="kv"><span>备份位置</span><span class="mono" title={snap.dest_dir ?? ""}>{shortPath(snap.dest_dir)}</span></div>
      </div>
    {/if}

    <div class="actions">
      <button class="btn primary grow" onclick={() => api.revealDest()}>在访达中显示</button>
      <button class="btn" onclick={() => api.dismiss()}>完成</button>
    </div>
    <div class="footer caption muted">{finishedTime()} 完成 · Completed</div>
  {:else if phase === "failed"}
    <!-- 出错（设计稿：弹窗 · 空间不足） -->
    {@const f = snap.fail}
    <div class="header">
      <div class="plate tone-dg"><Icon name="card-alert" /></div>
      <div class="grow">
        <div class="h">{card}{cap}</div>
        <div class="caption" style="color: var(--dg)">已停止 · Stopped</div>
      </div>
    </div>

    <div class="callout tone-dg">
      <Icon name="alert-circle" color="var(--dg)" />
      <div class="body">
        <div class="h">{failTitle}</div>
        <div class="small">{failSub}</div>
      </div>
    </div>

    {#if snap.files_done > 0}
      <div class="callout card-like">
        <Icon name="check" size={16} color="var(--ok)" />
        <div class="body">
          <div class="h" style="color: var(--ok)">已完成的 {snap.files_done} 个文件已校验，可放心使用</div>
          <div class="small muted">{snap.files_done} finished files are verified and safe.</div>
        </div>
      </div>
    {/if}

    {#if density !== "compact" && snap.files_total > 0}
      <div class="card">
        <div class="kv"><span>已复制</span><span class="num">{snap.files_done} / {snap.files_total} 个文件</span></div>
        {#if f?.kind === "no_space"}
          <div class="kv"><span>待复制</span><span class="num">{fmtBytes(f.pending_bytes)}</span></div>
          <div class="kv"><span>{targetName} 可用</span><span class="num">{fmtBytes(f.available_bytes)}</span></div>
        {:else}
          <div class="kv"><span>待复制</span><span class="num">{fmtBytes(Math.max(0, snap.bytes_total - snap.bytes_done))}</span></div>
        {/if}
      </div>
    {/if}

    <div class="actions">
      {#if f?.kind === "no_space" || f?.kind === "target_lost"}
        <button class="btn primary grow" onclick={chooseOtherTarget}>选择其他目标盘…</button>
        <button class="btn" onclick={() => api.retry()}>重试</button>
      {:else if f?.kind === "no_target"}
        <button class="btn primary grow" onclick={chooseOtherTarget}>选择备份目标…</button>
        <button class="btn" onclick={() => api.dismiss()}>取消</button>
      {:else if f?.kind === "card_removed"}
        <button class="btn primary grow" onclick={() => api.dismiss()}>完成</button>
      {:else}
        <button class="btn primary grow" onclick={() => api.retry()}>重试</button>
        <button class="btn" onclick={() => api.dismiss()}>完成</button>
      {/if}
    </div>
    <div class="footer caption muted">
      {f?.kind === "card_removed" ? "重新插卡即可自动续传 · Reinsert to resume" : "原文件仍在卡上 · Originals remain on the card"}
    </div>
  {:else if phase === "waiting_for_target"}
    <div class="header">
      <div class="plate tone-wn"><Icon name="card-clock" /></div>
      <div class="grow">
        <div class="h">{card}{cap}</div>
        <div class="caption" style="color: var(--wn)">等待目标盘 · Waiting</div>
      </div>
    </div>
    <div class="callout tone-wn">
      <Icon name="disk" color="var(--wn)" />
      <div class="body">
        <div class="h">未找到 {targetName}，接入后将自动继续，卡未弹出</div>
        <div class="small">Waiting for {targetName}. Will resume when connected.</div>
      </div>
    </div>
    <div class="actions">
      <button class="btn primary grow" onclick={chooseOtherTarget}>选择其他目标盘…</button>
      <button class="btn" onclick={() => api.dismiss()}>取消</button>
    </div>
    <div class="footer caption muted">原文件仍在卡上 · Originals remain on the card</div>
  {/if}
</div>

<style>
  .panel {
    width: 360px;
    background: var(--s1);
    border-radius: var(--r-xl);
    padding: 16px;
    display: flex;
    flex-direction: column;
    gap: 12px;
    overflow: hidden;
  }
  .header {
    display: flex;
    align-items: center;
    gap: 12px;
    min-height: 44px;
  }
  .grow {
    flex-grow: 1;
    min-width: 0;
  }
  .h {
    font-weight: 600;
  }
  .header .h {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .hero {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 4px 4px 4px 0;
  }
  .hero-text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .speed {
    display: flex;
    align-items: baseline;
    gap: 4px;
  }
  .display {
    font-size: 28px;
    line-height: 34px;
    font-weight: 600;
    letter-spacing: -0.2px;
  }
  .current {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .file-bar {
    height: 4px;
    margin-top: 4px;
    border-radius: 2px;
    background: var(--s3);
    overflow: hidden;
  }
  .file-bar > div {
    height: 100%;
    border-radius: 2px;
    background: var(--ac);
    transition: width 0.15s linear;
  }
  .file-bar.indeterminate > div {
    animation: slide 1.2s ease-in-out infinite;
  }
  @keyframes slide {
    from {
      transform: translateX(-100%);
    }
    to {
      transform: translateX(340%);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .file-bar.indeterminate > div {
      animation: none;
      width: 100% !important;
      opacity: 0.4;
    }
  }
  .ellipsis {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .footer {
    text-align: center;
  }
  .pass {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    color: var(--ok);
    font-weight: 600;
  }
  .card-like {
    background: var(--s2);
    align-items: flex-start;
    gap: 8px;
  }
  .card-like :global(svg) {
    margin-top: 1px;
  }
  .empty {
    background: var(--s2);
    border-radius: var(--r-lg);
    padding: 20px 12px;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 4px;
    text-align: center;
  }
  .status {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--ok);
    flex-shrink: 0;
  }
  .dot.off {
    background: transparent;
    border: 1.5px solid var(--wn);
  }
</style>
