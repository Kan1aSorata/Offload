<script lang="ts">
  let {
    checked,
    label,
    disabled = false,
    onchange,
  }: { checked: boolean; label: string; disabled?: boolean; onchange: (v: boolean) => void } = $props();
</script>

<button
  class="switch"
  class:on={checked}
  role="switch"
  aria-checked={checked}
  aria-label={label}
  {disabled}
  onclick={() => onchange(!checked)}
>
  <span class="knob"></span>
</button>

<style>
  .switch {
    position: relative;
    width: 40px;
    height: 24px;
    padding: 0;
    border: 1.5px solid var(--bs);
    border-radius: 999px;
    background: var(--s3);
    flex-shrink: 0;
    transition: background 0.15s ease;
  }
  .switch.on {
    border-color: var(--ac);
    background: var(--ac);
  }
  .knob {
    position: absolute;
    top: 1.5px;
    left: 1.5px;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: var(--bs);
    transition: transform 0.15s ease;
  }
  .switch.on .knob {
    transform: translateX(16px);
    background: var(--onac);
  }
</style>
