<script lang="ts">
  import { tick } from 'svelte';
  import { t } from 'svelte-i18n';
  import * as ipc from '../lib/ipc';
  import type { WindowInfo } from '../lib/types';

  let {
    open = $bindable(false),
    onselect,
  }: {
    open?: boolean;
    onselect: (w: WindowInfo) => void;
  } = $props();

  let windows = $state<WindowInfo[]>([]);
  let loading = $state(false);
  let loadError = $state<string | null>(null);
  let search = $state('');
  let dialogEl = $state<HTMLDivElement>();
  let searchInput = $state<HTMLInputElement>();
  let previousFocus = $state<HTMLElement | null>(null);

  let filtered = $derived.by(() => {
    const q = search.trim().toLowerCase();
    if (!q) return windows;
    return windows.filter(
      (w) => w.title.toLowerCase().includes(q) || w.exeName.toLowerCase().includes(q),
    );
  });

  async function load() {
    loading = true;
    try {
      windows = await ipc.listWindows();
      loadError = null;
    } catch (e) {
      windows = [];
      loadError = String(e);
    } finally {
      loading = false;
    }
  }

  function close() {
    if (loading) return;
    open = false;
  }

  function onkeydown(event: KeyboardEvent) {
    if (!open) return;
    if (event.key === 'Escape') {
      event.stopPropagation();
      close();
      return;
    }
    // 焦點陷阱
    if (event.key === 'Tab' && dialogEl) {
      const focusable = dialogEl.querySelectorAll<HTMLElement>(
        'input:not([disabled]), button:not([disabled])',
      );
      if (focusable.length < 2) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    }
  }

  // 開啟時記錄焦點並聚焦搜尋框；關閉時還原
  $effect(() => {
    if (open) {
      previousFocus = document.activeElement as HTMLElement | null;
      search = '';
      load();
      tick().then(() => searchInput?.focus());
    } else if (previousFocus) {
      previousFocus.focus();
      previousFocus = null;
    }
  });
</script>

<svelte:window {onkeydown} />

{#if open}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div class="overlay" role="presentation" onclick={close}>
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <div
      class="dialog"
      role="dialog"
      aria-modal="true"
      aria-label={$t('browse.title')}
      tabindex="-1"
      bind:this={dialogEl}
      onclick={(e) => e.stopPropagation()}
    >
      <div class="head">
        <h3>{$t('browse.title')}</h3>
        <button onclick={close} disabled={loading} aria-label={$t('browse.close')}>
          <svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true"><path d="M19 6.41L17.59 5 12 10.59 6.41 5 5 6.41 10.59 12 5 17.59 6.41 19 12 13.41 17.59 19 19 17.59 13.41 12z" fill="currentColor"/></svg>
        </button>
      </div>
      <div class="tools">
        <input
          type="text"
          placeholder={$t('browse.search')}
          bind:value={search}
          bind:this={searchInput}
        />
        <button onclick={load} disabled={loading}>{$t('browse.refresh')}</button>
      </div>
      <div class="list" role="list" aria-label={$t('browse.title')}>
        {#if loading}
          <div class="empty hint">…</div>
        {:else if loadError}
          <div class="empty hint" role="alert">
            {$t('browse.loadFailed', { values: { error: loadError } })}
            <button class="small" onclick={load}>{$t('browse.retry')}</button>
          </div>
        {:else if filtered.length === 0}
          <div class="empty hint">{$t('browse.empty')}</div>
        {:else}
          {#each filtered as w (w.hwnd)}
            <div class="item" role="listitem" aria-label={`${w.title} — ${w.exeName}`}>
              {#if w.iconPng}
                <img src="data:image/png;base64,{w.iconPng}" alt="" width="20" height="20" />
              {:else}
                <div class="no-icon"></div>
              {/if}
              <div class="meta">
                <div class="title">{w.title}</div>
                <div class="hint">{w.exeName} · PID {w.pid}</div>
              </div>
              {#if w.alreadyHasRule}
                <button class="small" disabled>{$t('browse.hasRule')}</button>
              {:else if !w.exePath}
                <button class="small" disabled title={w.exeName}>{$t('browse.protected')}</button>
              {:else}
                <button class="small primary" onclick={() => onselect(w)}>{$t('browse.select')}</button>
              {/if}
            </div>
          {/each}
        {/if}
      </div>
    </div>
  </div>
{/if}

<style>
  .overlay {
    position: fixed;
    inset: 0;
    z-index: 100;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: var(--space-5);
    background: var(--overlay);
    backdrop-filter: blur(4px);
  }

  .dialog {
    width: min(560px, 100%);
    max-height: 520px;
    display: flex;
    flex-direction: column;
    background: var(--surface-1);
    border: 1px solid var(--border-default);
    border-radius: var(--radius-xl);
    box-shadow: var(--shadow-lg);
    padding: var(--space-5);
  }

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: var(--space-4);
  }

  h3 {
    margin: 0;
    font-size: 15px;
  }

  .tools {
    display: flex;
    gap: var(--space-2);
    margin-bottom: var(--space-4);
  }

  .tools input {
    flex: 1;
  }

  .list {
    flex: 1;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .item {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: 8px var(--space-3);
    border-radius: var(--radius-md);
    background: var(--surface-2);
    border: 1px solid transparent;
    transition: border-color var(--transition-fast);
  }

  .item:hover {
    border-color: var(--border-default);
  }

  .item img {
    flex-shrink: 0;
  }

  .no-icon {
    width: 20px;
    height: 20px;
    border-radius: var(--radius-xs);
    background: var(--border-subtle);
    flex-shrink: 0;
  }

  .meta {
    flex: 1;
    min-width: 0;
  }

  .title {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 13px;
  }

  .empty {
    text-align: center;
    padding: var(--space-6);
  }
</style>
