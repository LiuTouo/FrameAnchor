<script lang="ts">
  import { onMount } from 'svelte';
  import { listen } from '@tauri-apps/api/event';
  import { locale, t } from 'svelte-i18n';
  import Dashboard from './pages/Dashboard.svelte';
  import Rules from './pages/Rules.svelte';
  import SettingsPage from './pages/Settings.svelte';
  import * as ipc from './lib/ipc';
  import { topology, rules, settings, applied, usage, updateState, isPortable } from './lib/stores';
  import type { AppliedProcess, UpdateState } from './lib/types';
  import { checkForUpdates, installUpdate } from './lib/updater';

  type Tab = 'dashboard' | 'rules' | 'settings';
  let tab = $state<Tab>('dashboard');

  // 啟動時找到的更新橫幅：本機 dismiss 旗標，不影響 store 狀態
  let updateBannerDismissed = $state(false);

  onMount(() => {
    let unlisteners: Array<() => void> = [];
    (async () => {
      topology.set(await ipc.getTopology());
      rules.set(await ipc.getRules());
      const s = await ipc.getSettings();
      settings.set(s);
      locale.set(s.language);
      applied.set(await ipc.getApplied());

      // 取得版本資訊與可攜版旗標
      const info = await ipc.getUpdateInfo();
      isPortable.set(info.portable);

      // 初始化 updateState 的 currentVersion
      updateState.set({
        status: 'Idle',
        latestVersion: null,
        currentVersion: info.version,
        progress: null,
        error: null,
      });

      // 事件監聽（必須在檢查更新前註冊，避免 race）
      unlisteners.push(await listen<number[]>('usage-update', (e) => usage.set(e.payload)));
      unlisteners.push(
        await listen<AppliedProcess[]>('applied-update', (e) => applied.set(e.payload)),
      );
      unlisteners.push(
        await listen<UpdateState>('update-state', (e) => updateState.set(e.payload)),
      );

      // 啟動時自動檢查更新（匯流至 updater 模組）
      await checkForUpdates();
    })();
    return () => unlisteners.forEach((u) => u());
  });

  /** 橫幅「安裝」按鈕：確認後呼叫共享安裝流程 */
  async function bannerInstall() {
    const curState = $updateState;
    if (!curState || curState.status !== 'Available') return;

    const title = $t('settings.updateConfirmTitle') as string;
    const body = $t('settings.updateConfirmBody', {
      values: { version: curState.latestVersion ?? '', current: curState.currentVersion },
    }) as string;
    if (!window.confirm(`${title}\n\n${body}`)) return;

    await installUpdate();
  }
</script>

<div class="shell">
  <nav>
    <div class="logo">Frame<span>Anchor</span></div>
    <button class:active={tab === 'dashboard'} onclick={() => (tab = 'dashboard')}>
      {$t('nav.dashboard')}
    </button>
    <button class:active={tab === 'rules'} onclick={() => (tab = 'rules')}>
      {$t('nav.rules')}
    </button>
    <button class:active={tab === 'settings'} onclick={() => (tab = 'settings')}>
      {$t('nav.settings')}
    </button>
  </nav>
  <main>
    {#if $updateState?.status === 'Available' && !updateBannerDismissed}
      <div class="update-banner">
        <span>
          {$t('update.bannerText', {
            values: { version: $updateState.latestVersion ?? '', current: $updateState.currentVersion },
          })}
        </span>
        <div class="update-banner-actions">
          <button class="primary" onclick={bannerInstall}>
            {$t('update.bannerInstall')}
          </button>
          <button onclick={() => (updateBannerDismissed = true)}>
            {$t('update.bannerDismiss')}
          </button>
        </div>
      </div>
    {/if}
    {#if tab === 'dashboard'}
      <Dashboard />
    {:else if tab === 'rules'}
      <Rules />
    {:else}
      <SettingsPage />
    {/if}
  </main>
</div>

<style>
  .shell {
    display: flex;
    height: 100%;
  }
  nav {
    width: 150px;
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 14px 10px;
    background: var(--panel);
    border-right: 1px solid var(--border);
  }
  .logo {
    font-weight: 700;
    font-size: 15px;
    margin-bottom: 16px;
    padding: 0 6px;
  }
  .logo span {
    color: var(--accent);
  }
  nav button {
    text-align: left;
    background: transparent;
    border: none;
    padding: 8px 10px;
    border-radius: 6px;
    color: var(--muted);
  }
  nav button:hover {
    color: var(--text);
    background: var(--panel-2);
  }
  nav button.active {
    color: var(--text);
    background: var(--panel-2);
    box-shadow: inset 2px 0 0 var(--accent);
  }
  main {
    flex: 1;
    overflow-y: auto;
    padding: 18px 20px;
  }
  .update-banner {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 10px 14px;
    margin-bottom: 14px;
    background: var(--panel);
    border: 1px solid var(--accent);
    border-radius: 8px;
    font-size: 13px;
  }
  .update-banner-actions {
    display: flex;
    gap: 8px;
    flex-shrink: 0;
  }
  .update-banner button {
    padding: 4px 12px;
    border-radius: 5px;
    font-size: 12px;
    cursor: pointer;
  }
  .update-banner button.primary {
    background: var(--accent);
    color: #fff;
    border: none;
  }
  .update-banner button:not(.primary) {
    background: transparent;
    color: var(--muted);
    border: 1px solid var(--border);
  }
</style>
