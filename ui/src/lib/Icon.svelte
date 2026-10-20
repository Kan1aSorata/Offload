<script lang="ts">
  // 设计稿中的线性图标，20×20 视框，1.5 描边
  type Name =
    | "card-up"
    | "card-check"
    | "card-alert"
    | "card-clock"
    | "card-scan"
    | "pause"
    | "play"
    | "gear"
    | "folder"
    | "check"
    | "check-circle"
    | "alert-circle"
    | "lock"
    | "disk"
    | "circle"
    | "info"
    | "close";

  let { name, size = 20, color = "currentColor", width = 1.5 }: { name: Name; size?: number; color?: string; width?: number } = $props();
</script>

<svg
  width={size}
  height={size}
  viewBox="0 0 20 20"
  fill="none"
  stroke={color}
  stroke-width={width}
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
>
  {#if name.startsWith("card")}
    <path d="M5 2.5H12L16.5 7V16A1.5 1.5 0 0 1 15 17.5H5A1.5 1.5 0 0 1 3.5 16V4A1.5 1.5 0 0 1 5 2.5Z" />
  {/if}
  {#if name === "card-up"}
    <path d="M10 14.5V9M7.5 11.2L10 8.7l2.5 2.5" />
  {:else if name === "card-check"}
    <path d="M7.3 12l2 2 3.6-4.2" />
  {:else if name === "card-alert"}
    <path d="M10 8.5v3.5M10 14.4v.1" />
  {:else if name === "card-clock"}
    <circle cx="10" cy="12" r="3.2" /><path d="M10 10.4V12l1.1.8" />
  {:else if name === "card-scan"}
    <path d="M7 10.5h6M7 13h4" />
  {:else if name === "pause"}
    <path d="M7.5 5.5v9M12.5 5.5v9" />
  {:else if name === "play"}
    <path d="M7 5.2v9.6L14.6 10Z" />
  {:else if name === "gear"}
    <circle cx="10" cy="10" r="2.5" />
    <path
      d="M10 2.8v1.7M10 15.5v1.7M17.2 10h-1.7M4.5 10H2.8M15.1 4.9l-1.2 1.2M6.1 13.9l-1.2 1.2M15.1 15.1l-1.2-1.2M6.1 6.1L4.9 4.9"
    />
  {:else if name === "folder"}
    <path d="M2.8 6V15a1.5 1.5 0 0 0 1.5 1.5h11.4a1.5 1.5 0 0 0 1.5-1.5V7.8a1.5 1.5 0 0 0-1.5-1.5H10L8.3 4.5H4.3A1.5 1.5 0 0 0 2.8 6Z" />
  {:else if name === "check"}
    <path d="M5 10.5l3.2 3.2L15 6.5" />
  {:else if name === "check-circle"}
    <circle cx="10" cy="10" r="7.5" /><path d="M6.8 10.2l2.3 2.3 4-4.6" />
  {:else if name === "alert-circle"}
    <circle cx="10" cy="10" r="7.5" /><path d="M10 6.3V10.5M10 13.3v.1" />
  {:else if name === "info"}
    <circle cx="10" cy="10" r="7.5" /><path d="M10 9v4.5M10 6.6v.1" />
  {:else if name === "lock"}
    <rect x="4.5" y="9" width="11" height="8" rx="2" /><path d="M7 9V6.5a3 3 0 0 1 6 0V9" />
  {:else if name === "disk"}
    <rect x="2.8" y="5.5" width="14.4" height="9" rx="2" /><path d="M13.5 10h.1M6 10h4" />
  {:else if name === "circle"}
    <circle cx="10" cy="10" r="7.5" />
  {:else if name === "close"}
    <path d="M6 6l8 8M14 6l-8 8" />
  {/if}
</svg>
