<script lang="ts">
  // 进度环：r=34，描边 8，与设计稿一致；indeterminate 时转圈
  let { pct = 0, indeterminate = false, label = "" }: { pct?: number; indeterminate?: boolean; label?: string } = $props();
  const C = 2 * Math.PI * 34;
  let offset = $derived(indeterminate ? C * 0.72 : C * (1 - Math.max(0, Math.min(100, pct)) / 100));
</script>

<div class="ring" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow={indeterminate ? undefined : pct} aria-label={label}>
  <svg width="84" height="84" viewBox="0 0 84 84" fill="none" aria-hidden="true" class:spin={indeterminate}>
    <circle cx="42" cy="42" r="34" stroke="var(--s3)" stroke-width="8" />
    <circle
      cx="42"
      cy="42"
      r="34"
      stroke="var(--ac)"
      stroke-width="8"
      stroke-linecap="round"
      stroke-dasharray={C}
      stroke-dashoffset={offset}
      class="bar"
    />
  </svg>
  <div class="center num">
    {#if !indeterminate}{pct}%{/if}
  </div>
</div>

<style>
  .ring {
    position: relative;
    width: 84px;
    height: 84px;
    flex-shrink: 0;
  }
  svg {
    transform: rotate(-90deg);
  }
  svg.spin {
    animation: spin 1.1s linear infinite;
  }
  .bar {
    transition: stroke-dashoffset 0.25s ease;
  }
  .center {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 17px;
    line-height: 22px;
    font-weight: 600;
  }
  @keyframes spin {
    from {
      transform: rotate(-90deg);
    }
    to {
      transform: rotate(270deg);
    }
  }
</style>
