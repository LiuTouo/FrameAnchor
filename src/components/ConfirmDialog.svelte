<script lang="ts">
  let {
    open = $bindable(false),
    title,
    message,
    detail = null,
    confirmLabel,
    cancelLabel,
    danger = false,
    busy = false,
    onconfirm,
    oncancel,
  }: {
    open?: boolean;
    title: string;
    message: string;
    detail?: string | null;
    confirmLabel: string;
    cancelLabel: string;
    danger?: boolean;
    busy?: boolean;
    onconfirm: () => void | Promise<void>;
    oncancel?: () => void;
  } = $props();

  function close() {
    if (!busy) {
      open = false;
      oncancel?.();
    }
  }

  function onkeydown(event: KeyboardEvent) {
    if (open && event.key === 'Escape') close();
  }
</script>

<svelte:window {onkeydown} />

{#if open}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div class="overlay" role="presentation" onclick={close}>
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <div
      class:danger
      class="dialog"
      role="alertdialog"
      tabindex="-1"
      aria-modal="true"
      aria-labelledby="confirm-dialog-title"
      aria-describedby="confirm-dialog-message"
      onclick={(event) => event.stopPropagation()}
    >
      <div class="icon" aria-hidden="true">{danger ? '!' : '?'}</div>
      <div class="content">
        <h2 id="confirm-dialog-title">{title}</h2>
        <p id="confirm-dialog-message">{message}</p>
        {#if detail}<p class="detail">{detail}</p>{/if}
        <div class="actions">
          <button onclick={close} disabled={busy}>{cancelLabel}</button>
          <button class:danger-button={danger} class:primary={!danger} onclick={onconfirm} disabled={busy}>
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  </div>
{/if}

<style>
  .overlay {
    position: fixed;
    inset: 0;
    z-index: 200;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 20px;
    background: rgba(5, 10, 18, 0.72);
    backdrop-filter: blur(2px);
  }
  .dialog {
    display: flex;
    gap: 14px;
    width: min(440px, 100%);
    padding: 18px;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    box-shadow: 0 18px 50px rgba(0, 0, 0, 0.38);
  }
  .dialog.danger { border-color: color-mix(in srgb, var(--danger) 55%, var(--border)); }
  .icon {
    display: grid;
    place-items: center;
    flex: 0 0 28px;
    width: 28px;
    height: 28px;
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 16%, transparent);
    border: 1px solid color-mix(in srgb, var(--accent) 45%, transparent);
    border-radius: 50%;
    font-weight: 700;
  }
  .danger .icon {
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 14%, transparent);
    border-color: color-mix(in srgb, var(--danger) 45%, transparent);
  }
  .content { flex: 1; min-width: 0; }
  h2 { margin: 3px 0 8px; font-size: 15px; }
  p { margin: 0; color: var(--text); line-height: 1.55; white-space: pre-line; }
  .detail { margin-top: 6px; color: var(--muted); font-size: 12px; }
  .actions { display: flex; justify-content: flex-end; gap: 8px; margin-top: 18px; }
  .danger-button { color: #fff; background: var(--danger); border-color: var(--danger); }
</style>
