<script lang="ts">
  import { locale, t } from 'svelte-i18n';
  import * as ipc from '../lib/ipc';
  import { settings, updateState, isPortable } from '../lib/stores';
  import type { Settings, UpdateStatus } from '../lib/types';
  import { checkForUpdates, installUpdate } from '../lib/updater';

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

    const title = $t('settings.updateConfirmTitle') as string;
    const body = $t('settings.updateConfirmBody', {
      values: { version: curState.latestVersion ?? '', current: curState.currentVersion },
    }) as string;
    if (!window.confirm(`${title}\n\n${body}`)) return;

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
  <h2>{$t('settings.general')}</h2>
  <div class="panel">
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
    <label class="opt">
      <input
        type="checkbox"
        checked={$settings.showAdvancedPriorities}
        onchange={(e) => save({ showAdvancedPriorities: e.currentTarget.checked })}
      />
      <span>{$t('settings.showAdvanced')}</span>
    </label>

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
  </div>

  <h2>{$t('settings.about')}</h2>
  <div class="panel">
    <div class="opt row">
      <span class="hint">FrameAnchor · {$t('settings.version')} {$updateState?.currentVersion ?? '0.0.0'}</span>
      {#if $isPortable}
        <span class="hint tag">{$t('settings.portableBuild')}</span>
      {/if}
    </div>

    <!-- 更新狀態與控制 -->
    <div class="opt row">
      {#if $updateState && $updateState.status !== 'Idle'}
        <span class="hint">
          {statusLabel($updateState.status)}
          {#if $updateState.latestVersion && $updateState.status === 'Available'}
            · {$updateState.latestVersion}
          {/if}
          {#if $updateState.progress !== null && $updateState.status === 'Downloading'}
            · {$updateState.progress}%
          {/if}
        </span>
      {/if}

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
        <span class="hint error">{$t('settings.updateErrorDetail', { values: { error: $updateState.error } })}</span>
      </div>
    {/if}

    <div class="opt row">
      <span class="hint">{dataDir}</span>
      <button onclick={() => ipc.openDataFolder()}>{$t('settings.dataFolder')}</button>
    </div>
  </div>
{/if}

<style>
  h2 {
    font-size: 14px;
    margin: 4px 0 10px;
  }
  h2:not(:first-child) {
    margin-top: 24px;
  }
  .panel {
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 14px 16px;
    display: flex;
    flex-direction: column;
    gap: 12px;
    max-width: 560px;
  }
  .opt {
    display: flex;
    align-items: center;
    gap: 8px;
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
    gap: 6px;
  }
  input[type='range'] {
    accent-color: var(--accent);
  }
  .tag {
    background: var(--panel-2);
    padding: 1px 6px;
    border-radius: 4px;
    font-size: 11px;
  }
  .error {
    color: #f87171;
  }
</style>
