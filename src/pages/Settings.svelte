<script lang="ts">
  import { locale, t } from 'svelte-i18n';
  import * as ipc from '../lib/ipc';
  import { settings } from '../lib/stores';
  import type { Settings } from '../lib/types';

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
      await ipc.setAutostart(enable); // 後端同步寫 settings.start_with_windows
      const current = $settings;
      if (current) settings.set({ ...current, startWithWindows: enable });
    } catch (e) {
      console.error('set_autostart failed', e);
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
      <span class="hint">FrameAnchor · {$t('settings.version')} 0.1.0</span>
    </div>
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
</style>
