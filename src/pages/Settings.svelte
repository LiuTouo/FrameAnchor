<script lang="ts">
  import { locale, t } from 'svelte-i18n';
  import * as ipc from '../lib/ipc';
  import { settings, updateState, isPortable } from '../lib/stores';
  import type { Settings, UpdateStatus } from '../lib/types';
  import { checkForUpdates, installUpdate } from '../lib/updater';
  import ConfirmDialog from '../components/ConfirmDialog.svelte';

  let updateConfirmOpen = $state(false);

  // busy 狀態直接從 updateState store 推導
  let checking = $derived($updateState?.status === 'Checking');
  let installing = $derived(
    $updateState?.status === 'Downloading' || $updateState?.status === 'Installing',
  );

  async function save(partial: Partial<Settings>) {
    const current = $settings;
    if (!current) return;
    const next = { ...current, ...partial };
    settings.set(next);
    try {
      await ipc.saveSettings(next);
      if (partial.language) locale.set(next.language);
    } catch (e) {
      console.error('save_settings failed', e);
    }
  }

  async function toggleAutostart(enable: boolean) {
    try {
      await ipc.setAutostart(enable);
      const current = $settings;
      if (current) settings.set({ ...current, startWithWindows: enable });
    } catch (e) {
      console.error('set_autostart failed', e);
    }
  }

  /// 手動檢查更新
  async function manualCheck() {
    await checkForUpdates();
  }

  /// 執行更新（含確認對話框）
  async function doUpdate() {
    const curState = $updateState;
    if (!curState || curState.status !== 'Available') return;
    updateConfirmOpen = true;
  }

  async function confirmUpdate() {
    updateConfirmOpen = false;
    await installUpdate();
  }

  /// from update-state event 或手動設定
  function statusLabel(s: UpdateStatus | null): string {
    if (!s) return '';
    switch (s) {
      case 'Checking': return $t('settings.updateChecking') as string;
      case 'UpToDate': return $t('settings.updateUpToDate') as string;
      case 'Available': return $t('settings.updateAvailable') as string;
      case 'Downloading': return $t('settings.updateDownloading') as string;
      case 'Installing': return $t('settings.updateInstalling') as string;
      case 'Error': return $t('settings.updateError') as string;
      default: return '';
    }
  }

  const dataDir = '%APPDATA%\\FrameAnchor';
</script>

{#if $settings}
  <!-- ── 一般 ── -->
  <h2>{$t('settings.general')}</h2>
  <div class="section">
    <label class="opt">
      <input
        type="checkbox"
        checked={$settings.startWithWindows}
        onchange={(e) => toggleAutostart(e.currentTarget.checked)}
      />
      <span>{$t('settings.autostart')}</span>
    </label>
    <label class="opt">
      <input
        type="checkbox"
        checked={$settings.startMinimized}
        onchange={(e) => save({ startMinimized: e.currentTarget.checked })}
      />
      <span>{$t('settings.startMinimized')}</span>
    </label>
    <label class="opt">
      <input
        type="checkbox"
        checked={$settings.closeToTray}
        onchange={(e) => save({ closeToTray: e.currentTarget.checked })}
      />
      <span>{$t('settings.closeToTray')}</span>
    </label>

    <div class="opt col">
      <span>
        {$t('settings.pollInterval')}：
        {$t('settings.seconds', { values: { value: ($settings.pollIntervalMs / 1000).toFixed(1) } })}
      </span>
      <input
        type="range"
        min="500"
        max="5000"
        step="500"
        value={$settings.pollIntervalMs}
        onchange={(e) => save({ pollIntervalMs: Number(e.currentTarget.value) })}
        oninput={(e) =>
          settings.update((s) => (s ? { ...s, pollIntervalMs: Number(e.currentTarget.value) } : s))}
      />
      <span class="hint">{$t('settings.pollHint')}</span>
    </div>

    <label class="opt">
      <input
        type="checkbox"
        checked={$settings.showAdvancedPriorities}
        onchange={(e) => save({ showAdvancedPriorities: e.currentTarget.checked })}
      />
      <span>{$t('settings.showAdvanced')}</span>
    </label>
  </div>

  <!-- ── 外觀 ── -->
  <h2>{$t('settings.appearance')}</h2>
  <div class="section">
    <div class="opt row">
      <span>{$t('settings.language')}</span>
      <select
        value={$settings.language}
        onchange={(e) => save({ language: e.currentTarget.value })}
      >
        <option value="zh-TW">繁體中文</option>
        <option value="en">English</option>
      </select>
    </div>

    <div class="opt row">
      <span>{$t('settings.theme')}</span>
      <select
        value={$settings.theme}
        onchange={(e) => save({ theme: e.currentTarget.value as 'Dark' | 'Light' })}
      >
        <option value="Dark">{$t('settings.themeDark')}</option>
        <option value="Light">{$t('settings.themeLight')}</option>
      </select>
    </div>
  </div>

  <!-- ── 更新與版本 ── -->
  <h2>{$t('settings.updateSection')}</h2>
  <div class="section">
    <div class="opt row">
      <span class="hint">
        FrameAnchor · {$t('settings.version')} {$updateState?.currentVersion ?? '0.0.0'}
        {#if $isPortable}
          <span class="tag">{$t('settings.portableBuild')}</span>
        {/if}
      </span>
    </div>

    <div class="opt row">
      <span class="hint">
        {#if $updateState && $updateState.status !== 'Idle'}
          {statusLabel($updateState.status)}
          {#if $updateState.latestVersion && $updateState.status === 'Available'}
            · {$updateState.latestVersion}
          {/if}
          {#if $updateState.progress !== null && $updateState.status === 'Downloading'}
            · {$updateState.progress}%
          {/if}
        {/if}
      </span>

      {#if $updateState?.status === 'Available'}
        <button onclick={doUpdate} disabled={installing}>
          {installing ? '…' : $t('settings.updateInstall')}
        </button>
      {:else if $updateState?.status !== 'Downloading' && $updateState?.status !== 'Installing'}
        <button onclick={manualCheck} disabled={checking || installing}>
          {checking ? '…' : $t('settings.updateCheck')}
        </button>
      {/if}
    </div>

    {#if $updateState?.error}
      <div class="opt">
        <span class="error-msg" role="alert">
          <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true"><path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm1 15h-2v-2h2v2zm0-4h-2V7h2v6z" fill="currentColor"/></svg>
          {$t('settings.updateErrorDetail', { values: { error: $updateState.error } })}
        </span>
      </div>
    {/if}
  </div>

  <!-- ── 資料 ── -->
  <h2>{$t('settings.data')}</h2>
  <div class="section">
    <div class="opt row">
      <span class="hint mono">{dataDir}</span>
      <button onclick={() => ipc.openDataFolder()}>{$t('settings.dataFolder')}</button>
    </div>
  </div>
{/if}

<ConfirmDialog
  bind:open={updateConfirmOpen}
  title={$t('settings.updateConfirmTitle') as string}
  message={$t('settings.updateConfirmBody', {
    values: { version: $updateState?.latestVersion ?? '', current: $updateState?.currentVersion ?? '' },
  }) as string}
  confirmLabel={$t('settings.updateInstall') as string}
  cancelLabel={$t('common.cancel') as string}
  onconfirm={confirmUpdate}
/>

<style>
  h2 {
    font-size: 14px;
    margin: 0 0 var(--space-3);
  }

  h2:not(:first-child) {
    margin-top: var(--space-6);
  }

  .section {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    max-width: 560px;
  }

  .opt {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    cursor: pointer;
  }

  .opt.row,
  .opt.col {
    cursor: default;
  }

  .opt.row {
    justify-content: space-between;
  }

  .opt.col {
    flex-direction: column;
    align-items: stretch;
    gap: var(--space-2);
  }

  input[type='range'] {
    accent-color: var(--accent);
  }

  .tag {
    background: var(--surface-2);
    padding: 1px 6px;
    border-radius: var(--radius-xs);
    font-size: 11px;
    margin-left: var(--space-2);
  }

  .error-msg {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
    color: var(--danger);
    font-size: 12px;
  }

  .mono {
    font-family: Consolas, 'Cascadia Code', monospace;
    font-size: 11px;
  }
</style>
