<script lang="ts">
  import { onMount } from "svelte";
  import { api, on, fmtBytes, type VolumeInfo, type Config } from "./lib/api";
  import Icon from "./lib/Icon.svelte";

  let step = $state(1);
  let volumes = $state<VolumeInfo[]>([]);
  let cfg = $state<Config | null>(null);
  /** 选中的外置盘挂载点；选了自定义文件夹时为 null */
  let selected = $state<string | null>(null);
  let busy = $state(false);
  let error = $state("");

  async function loadVolumes() {
    volumes = await api.listVolumes();
    cfg = await api.getSettings();
    if (selected && !volumes.some((v) => v.mount_point === selected)) selected = null;
    // 默认选中当前目标所在盘，否则选空间最大的那块
    if (!selected && !customFolder) {
      const current = volumes.find((v) => cfg?.target_path?.startsWith(v.mount_point + "/"));
      selected = (current ?? [...volumes].sort((a, b) => b.free_bytes - a.free_bytes)[0])?.mount_point ?? null;
    }
  }

  let customFolder = $derived(
    cfg?.target_path && !volumes.some((v) => cfg!.target_path === v.mount_point + "/Cards") && selected === null
      ? cfg.target_path
      : null,
  );

  onMount(() => {
    loadVolumes();
    // 引导期间插拔 SSD 时刷新列表
    const timer = setInterval(() => step === 2 && loadVolumes(), 2000);
    const sub = on("window-shown", () => {
      step = 1;
      loadVolumes();
    });
    return () => {
      clearInterval(timer);
      sub.then((u) => u());
    };
  });

  async function pickFolder() {
    error = "";
    const c = await api.pickTargetFolder();
    if (c) {
      cfg = c;
      selected = null;
    }
  }

  let canContinue = $derived(step !== 2 || selected !== null || customFolder !== null);

  async function next() {
    error = "";
    if (step === 2 && selected) {
      busy = true;
      try {
        cfg = await api.useVolume(selected);
      } catch (e) {
        error = String(e);
        busy = false;
        return;
      }
      busy = false;
    }
    if (step < 3) {
      step++;
      return;
    }
    busy = true;
    try {
      await api.completeOnboarding();
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }
</script>

<div class="wrap">
  <div class="top">
    <span class="caption muted">第 {step} 步，共 3 步</span>
    <div class="dots" aria-hidden="true">
      {#each [1, 2, 3] as i (i)}
        <span class="dot" class:on={i === step}></span>
      {/each}
    </div>
  </div>

  {#if step === 1}
    <div class="stack">
      <div class="hero tone-ac"><Icon name="card-check" size={64} width={1} /></div>
      <div class="text">
        <h1 class="title">插入相机卡，自动备份</h1>
        <p class="muted">Offload 在菜单栏安静等待。插入卡后立即复制并逐个校验，完成时弹出卡。</p>
        <p class="small muted">Insert a card to back it up. Verified, then ejected.</p>
      </div>
    </div>
  {:else if step === 2}
    <div class="stack">
      <div class="text">
        <h1 class="title">选择备份目标</h1>
        <p class="muted">备份将保存到这块外置 SSD 的 Cards 文件夹。之后可在设置中更改。</p>
        <p class="small muted">Choose the SSD to back up to.</p>
      </div>
      <div class="list" role="radiogroup" aria-label="备份目标">
        {#each volumes as v (v.mount_point)}
          <label class="opt" class:sel={selected === v.mount_point}>
            <input type="radio" name="target" value={v.mount_point} bind:group={selected} />
            <Icon name={selected === v.mount_point ? "check-circle" : "circle"} color={selected === v.mount_point ? "var(--acink)" : "var(--bs)"} />
            <span class="col">
              <span class:h={selected === v.mount_point}>{v.name}</span>
              <span class="small muted num">{fmtBytes(v.total_bytes)} · {fmtBytes(v.free_bytes)} 可用</span>
            </span>
            {#if selected === v.mount_point}<span class="caption picked">已选择</span>{/if}
          </label>
        {:else}
          <div class="opt empty">
            <Icon name="disk" color="var(--mut)" />
            <span class="col">
              <span>没有发现外置磁盘</span>
              <span class="small muted">接入 SSD 后会自动出现在这里</span>
            </span>
          </div>
        {/each}
        {#if customFolder}
          <div class="opt sel">
            <Icon name="check-circle" color="var(--acink)" />
            <span class="col">
              <span class="h">自定义文件夹</span>
              <span class="mono muted path">{customFolder}</span>
            </span>
            <span class="caption picked">已选择</span>
          </div>
        {/if}
      </div>
      <div>
        <button class="btn text" style="padding: 0" onclick={pickFolder}>选择其他文件夹…</button>
      </div>
    </div>
  {:else}
    <div class="stack">
      <div class="text">
        <h1 class="title">允许访问可移除卷宗</h1>
        <p class="muted">请在系统弹窗中选择“允许”，Offload 才能读取相机卡并写入 SSD。</p>
        <p class="small muted">Choose “Allow” so Offload can read the card and write to the SSD.</p>
      </div>
      <div class="callout tone-ac">
        <Icon name="lock" color="var(--acink)" />
        <div class="body">
          <span class="h">文件只在本机复制</span>
          <span class="small">不会上传，也不会读取卡以外的内容。</span>
        </div>
      </div>
      <p class="small muted">提示：在「图像捕捉」中把相机卡的“连接此设备时打开”设为“无”，可避免「照片」App 抢卡。</p>
    </div>
  {/if}

  {#if error}
    <p class="small" style="color: var(--dg)" role="alert">{error}</p>
  {/if}

  <div class="nav">
    {#if step > 1}
      <button class="btn" onclick={() => (step = Math.max(1, step - 1))}>返回</button>
    {/if}
    <span class="spacer"></span>
    <button class="btn primary" style="padding: 0 20px" disabled={!canContinue || busy} onclick={next}>
      {step === 3 ? "允许并开始" : "继续"}
    </button>
  </div>
</div>

<style>
  .wrap {
    min-height: 100vh;
    padding: 24px;
    display: flex;
    flex-direction: column;
    gap: 20px;
    background: var(--s1);
  }
  .top {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .dots {
    display: flex;
    gap: 6px;
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 999px;
    background: var(--hair);
    transition: width 0.2s ease;
  }
  .dot.on {
    width: 18px;
    background: var(--ac);
  }
  .stack {
    display: flex;
    flex-direction: column;
    gap: 16px;
    flex-grow: 1;
  }
  .hero {
    height: 132px;
    border-radius: var(--r-lg);
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .text {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  h1,
  p {
    margin: 0;
  }
  .list {
    background: var(--s2);
    border-radius: var(--r-lg);
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  .opt {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 12px;
  }
  .opt + .opt {
    border-top: 1px solid var(--hair);
  }
  .opt.sel {
    background: var(--acsoft);
  }
  .opt input {
    position: absolute;
    opacity: 0;
    pointer-events: none;
  }
  .opt:has(input:focus-visible) {
    outline: 2px solid var(--acink);
    outline-offset: -2px;
  }
  .col {
    flex-grow: 1;
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
  .picked {
    color: var(--acink);
    font-weight: 600;
  }
  .nav {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .spacer {
    flex-grow: 1;
  }
</style>
