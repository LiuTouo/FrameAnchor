<script lang="ts">
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
  let search = $state('');

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
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    if (open) {
      search = '';
      load();
    }
  });
</script>

{#if open}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div class="overlay" role="presentation" onclick={() => (open = false)}>
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <div class="dialog" role="dialog" aria-modal="true" tabindex="-1" onclick={(e) => e.stopPropagation()}>
      <div class="head">
        <h3>{$t('browse.title')}</h3>
        <button onclick={() => (open = false)}>✕</button>
      </div>
      <div class="tools">
        <input type="text" placeholder={$t('browse.search')} bind:value={search} />
        <button onclick={load} disabled={loading}>{$t('browse.refresh')}</button>
      </div>
      <div class="list">
        {#if filtered.length === 0}
          <div class="hint empty">{loading ? '…' : $t('browse.empty')}</div>
        {/if}
        {#each filtered as w (w.hwnd)}
          <div class="item">
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
              <button disabled>{$t('browse.hasRule')}</button>
            {:else if !w.exePath}
              <button disabled title={w.exeName}>{$t('browse.protected')}</button>
            {:else}
              <button class="primary" onclick={() => onselect(w)}>{$t('browse.select')}</button>
            {/if}
          </div>
        {/each}
      </div>
    </div>
  </div>
{/if}

<style>
  .overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.55);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
  }
  .dialog {
    width: 520px;
    max-height: 480px;
    display: flex;
    flex-direction: column;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 14px;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 10px;
  }
  h3 {
    margin: 0;
    font-size: 14px;
  }
  .tools {
    display: flex;
    gap: 8px;
    margin-bottom: 10px;
  }
  .tools input {
    flex: 1;
  }
  .list {
    flex: 1;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .item {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 6px 8px;
    border-radius: 6px;
    background: var(--panel-2);
  }
  .item img {
    flex-shrink: 0;
  }
  .no-icon {
    width: 20px;
    height: 20px;
    border-radius: 4px;
    background: var(--border);
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
  }
  .empty {
    text-align: center;
    padding: 24px;
  }
</style>
