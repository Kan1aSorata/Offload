<script lang="ts" generics="T extends string">
  let {
    value,
    options,
    label,
    onchange,
  }: { value: T; options: { value: T; label: string }[]; label: string; onchange: (v: T) => void } = $props();
</script>

<div class="seg" role="radiogroup" aria-label={label}>
  {#each options as o (o.value)}
    <button role="radio" aria-checked={value === o.value} class:active={value === o.value} onclick={() => onchange(o.value)}>
      {o.label}
    </button>
  {/each}
</div>

<style>
  .seg {
    display: flex;
    padding: 2px;
    gap: 2px;
    background: var(--s3);
    border-radius: var(--r-md);
    flex-shrink: 0;
  }
  button {
    height: 28px;
    padding: 0 12px;
    border: 0;
    border-radius: 8px;
    background: transparent;
    color: var(--ink);
    white-space: nowrap;
  }
  button.active {
    background: var(--ac);
    color: var(--onac);
    font-weight: 600;
  }
</style>
