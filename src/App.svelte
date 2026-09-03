<script lang="ts">
  import { onMount } from 'svelte';
  import { listen } from '@tauri-apps/api/event';
  import { locale, t } from 'svelte-i18n';
  import Dashboard from './pages/Dashboard.svelte';
  import GpuTest from './pages/GpuTest.svelte';
  import Rules from './pages/Rules.svelte';
  import SettingsPage from './pages/Settings.svelte';
  import ConfirmDialog from './components/ConfirmDialog.svelte';
  import * as ipc from './lib/ipc';
  import {
    applied,
    benchmarkProgress,
    benchmarkState,
    isPortable,
    rules,
    settings,
    topology,
    updateState,
    usage,
  } from './lib/stores';
  import type { AppliedProcess, BenchmarkProgress, UpdateState } from './lib/types';
  import { checkForUpdates, installUpdate } from './lib/updater';

  type Tab = 'dashboard' | 'rules' | 'gpu' | 'settings';
  let tab = $state<Tab>('dashboard');

  // 基準測試執行中 → 鎖定導覽，不能離開 GPU 測試頁
  const benchmarkRunning = $derived($benchmarkState?.status === 'Running');
  // compact progress 視窗模式（後端 windowLayout=CompactProgress）→ 隱藏側欄/橫幅
  const compact = $derived($benchmarkState?.windowLayout === 'CompactProgress');

  function switchTab(next: Tab) {
    if (benchmarkRunning && next !== 'gpu') return; // 執行中禁止離開
    tab = next;
  }

  function navDisabled(next: Tab) {
    return benchmarkRunning && next !== 'gpu';
  }

  // 進入 Running → 自動切到 GPU 測試頁（reload/reopen 也落在警告+進度+取消）
  $effect(() => {
    if ($benchmarkState?.status === 'Running') tab = 'gpu';
  });

  // 主題：監聽 settings store 變更，同步到 document.documentElement
  $effect(() => {
    const theme = $settings?.theme ?? 'Dark';
    document.documentElement.setAttribute('data-theme', theme.toLowerCase());
  });

  // 啟動時找到的更新橫幅：本機 dismiss 旗標，不影響 store 狀態
  let updateBannerDismissed = $state(false);
  let updateConfirmOpen = $state(false);

  onMount(() => {
    let unlisteners: Array<() => void> = [];
    (async () => {
      topology.set(await ipc.getTopology());
      rules.set(await ipc.getRules());
      const s = await ipc.getSettings();
      settings.set(s);
      locale.set(s.language);
      applied.set(await ipc.getApplied());

      // 重建基準測試執行期狀態（reload 後不重啟/不停止 session）
      benchmarkState.set(await ipc.getBenchmarkState());

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
      // GPU 基準測試進度事件 → 即時更新 state（後端仍是執行期唯一 owner）
      unlisteners.push(
        await listen<BenchmarkProgress>('gpu-benchmark-progress', (e) => {
          benchmarkProgress.set(e.payload);
          void ipc.getBenchmarkState().then((s) => benchmarkState.set(s));
        }),
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
    updateConfirmOpen = true;
  }

  async function confirmBannerInstall() {
    updateConfirmOpen = false;
    await installUpdate();
  }

  // ── 導覽項目定義（icon 為 inline SVG path data） ──
  const navItems: { tab: Tab; label: string; icon: string }[] = [
    {
      tab: 'dashboard',
      label: 'dashboard',
      icon: 'M3 13h8V3H3v10zm0 8h8v-6H3v6zm10 0h8V11h-8v10zm0-18v6h8V3h-8z',
    },
    {
      tab: 'rules',
      label: 'rules',
      icon: 'M19.14 12.94c.04-.3.06-.61.06-.94 0-.32-.02-.64-.07-.94l2.03-1.58a.49.49 0 00.12-.61l-1.92-3.32a.49.49 0 00-.59-.22l-2.39.96c-.5-.38-1.03-.7-1.62-.94L14.4 2.81a.48.48 0 00-.48-.31h-3.84a.48.48 0 00-.48.31l-.36 2.54c-.59.24-1.13.57-1.62.94l-2.39-.96a.49.49 0 00-.59.22L2.74 8.87c-.12.21-.08.47.12.61l2.03 1.58c-.05.3-.07.62-.07.94s.02.64.07.94l-2.03 1.58a.49.49 0 00-.12.61l1.92 3.32c.12.22.37.29.59.22l2.39-.96c.5.38 1.03.7 1.62.94l.36 2.54c.05.23.26.41.48.41h3.84c.24 0 .44-.18.48-.41l.36-2.54c.59-.24 1.13-.56 1.62-.94l2.39.96c.22.08.47 0 .59-.22l1.92-3.32c.12-.22.07-.47-.12-.61l-2.01-1.58zM12 15.6A3.6 3.6 0 1115.6 12 3.61 3.61 0 0112 15.6z',
    },
    {
      tab: 'gpu',
      label: 'gpuTest',
      icon: 'M21 3H3v18h18V3zm-2 16H5V5h14v14zm-4.5-7h-3v3h-2v-3h-3V9h3V6h2v3h3v2z',
    },
    {
      tab: 'settings',
      label: 'settings',
      icon: 'M12 15.5A3.5 3.5 0 018.5 12 3.5 3.5 0 0112 8.5a3.5 3.5 0 013.5 3.5 3.5 3.5 0 01-3.5 3.5zm7.43-2.53c.04-.32.07-.64.07-.97 0-.33-.03-.66-.07-.98l2.11-1.65c.19-.15.24-.42.12-.64l-2-3.46a.5.5 0 00-.61-.22l-2.49 1c-.52-.4-1.08-.73-1.69-.98l-.38-2.65A.49.49 0 0014 2h-4c-.25 0-.46.18-.49.42l-.38 2.65c-.61.25-1.17.59-1.69.98l-2.49-1a.5.5 0 00-.61.22l-2 3.46c-.13.22-.07.49.12.64l2.11 1.65c-.04.32-.07.65-.07.98 0 .33.03.66.07.98l-2.11 1.65c-.19.15-.24.42-.12.64l2 3.46a.5.5 0 00.61.22l2.49-1c.52.4 1.08.73 1.69.98l.38 2.65c.03.24.24.42.49.42h4c.25 0 .46-.18.49-.42l.38-2.65c.61-.25 1.17-.59 1.69-.98l2.49 1c.23.09.49 0 .61-.22l2-3.46c.13-.22.07-.49-.12-.64l-2.11-1.65zM12 17c-2.76 0-5-2.24-5-5s2.24-5 5-5 5 2.24 5 5-2.24 5-5 5z',
    },
  ];
</script>

<div class="shell" class:compact>
  <!-- 側欄導覽（compact progress 模式隱藏） -->
  {#if !compact}
  <nav class="sidebar" aria-label={$t('nav.settings')}>
    <div class="brand">
      <svg class="brand-icon" viewBox="0 0 24 24" aria-hidden="true">
        <path d="M12 2L2 7l10 5 10-5-10-5zM2 17l10 5 10-5M2 12l10 5 10-5" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
      </svg>
      <span class="brand-text">Frame<span class="brand-accent">Anchor</span></span>
    </div>

    <div class="nav-items">
      {#each navItems as item (item.tab)}
        <button
          class="nav-btn"
          class:active={tab === item.tab}
          disabled={navDisabled(item.tab)}
          onclick={() => switchTab(item.tab)}
          aria-current={tab === item.tab ? 'page' : undefined}
        >
          <svg viewBox="0 0 24 24" class="nav-icon" aria-hidden="true">
            <path d={item.icon} fill="currentColor"/>
          </svg>
          <span>{$t(`nav.${item.label}`)}</span>
        </button>
      {/each}
    </div>

    <div class="sidebar-footer">
      <span class="hint version-label">v{$updateState?.currentVersion ?? '0.0.0'}</span>
    </div>
  </nav>
  {/if}

  <!-- 主內容區 -->
  <main class="content">
    {#if $updateState?.status === 'Available' && !updateBannerDismissed && !compact}
      <div class="update-banner" role="status">
        <svg class="banner-icon" viewBox="0 0 24 24" aria-hidden="true">
          <path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm-2 15l-5-5 1.41-1.41L10 14.17l7.59-7.59L19 8l-9 9z" fill="currentColor"/>
        </svg>
        <span>
          {$t('update.bannerText', {
            values: { version: $updateState.latestVersion ?? '', current: $updateState.currentVersion },
          })}
        </span>
        <div class="banner-actions">
          <button class="small primary" onclick={bannerInstall}>
            {$t('update.bannerInstall')}
          </button>
          <button class="small" onclick={() => (updateBannerDismissed = true)}>
            {$t('update.bannerDismiss')}
          </button>
        </div>
      </div>
    {/if}

    <div class="page">
      {#if tab === 'dashboard'}
        <Dashboard />
      {:else if tab === 'rules'}
        <Rules />
      {:else if tab === 'gpu'}
        <GpuTest />
      {:else}
        <SettingsPage />
      {/if}
    </div>
  </main>
</div>

<ConfirmDialog
  bind:open={updateConfirmOpen}
  title={$t('settings.updateConfirmTitle') as string}
  message={$t('settings.updateConfirmBody', {
    values: { version: $updateState?.latestVersion ?? '', current: $updateState?.currentVersion ?? '' },
  }) as string}
  confirmLabel={$t('settings.updateInstall') as string}
  cancelLabel={$t('common.cancel') as string}
  onconfirm={confirmBannerInstall}
/>

<style>
  .shell {
    display: flex;
    height: 100%;
  }

  /* ── 側欄 ── */
  .sidebar {
    width: 216px;
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    background: var(--surface-1);
    border-right: 1px solid var(--border-subtle);
    padding: var(--space-4) var(--space-3);
    gap: var(--space-2);
  }

  .brand {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-1) var(--space-2) var(--space-5);
  }

  .brand-icon {
    width: 26px;
    height: 26px;
    color: var(--accent);
    flex-shrink: 0;
  }

  .brand-text {
    font-weight: var(--font-weight-semibold);
    font-size: 16px;
    letter-spacing: -0.01em;
  }

  .brand-accent {
    color: var(--accent);
  }

  /* ── 導覽按鈕 ── */
  .nav-items {
    display: flex;
    flex-direction: column;
    gap: 4px;
    flex: 1;
  }

  .nav-btn {
    position: relative;
    display: flex;
    align-items: center;
    gap: var(--space-3);
    width: 100%;
    text-align: left;
    background: transparent;
    border: none;
    border-radius: var(--radius-md);
    padding: 0 var(--space-3);
    height: 40px;
    color: var(--text-secondary);
    font-size: 13.5px;
    cursor: pointer;
    transition: background var(--transition-fast), color var(--transition-fast);
  }

  .nav-btn:hover:not(:disabled) {
    color: var(--text-primary);
    background: var(--surface-2);
  }

  .nav-btn:active:not(:disabled) {
    background: var(--surface-3);
  }

  .nav-btn.active {
    color: var(--text-primary);
    background: var(--accent-muted);
    font-weight: var(--font-weight-medium);
  }

  .nav-btn.active::before {
    content: '';
    position: absolute;
    left: 0;
    top: 9px;
    bottom: 9px;
    width: 3px;
    border-radius: var(--radius-full);
    background: var(--accent);
  }

  .nav-btn:disabled {
    opacity: 0.35;
    cursor: default;
  }

  .nav-icon {
    width: 18px;
    height: 18px;
    flex-shrink: 0;
    color: var(--text-muted);
    transition: color var(--transition-fast);
  }

  .nav-btn.active .nav-icon {
    color: var(--accent);
  }

  /* ── 側欄底部 ── */
  .sidebar-footer {
    padding: var(--space-3) var(--space-2) 0;
    border-top: 1px solid var(--border-subtle);
    margin-top: var(--space-2);
  }

  .version-label {
    font-size: 11px;
  }

  /* ── 主內容區 ── */
  .content {
    flex: 1;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    min-width: 0;
  }

  .page {
    flex: 1;
    overflow-y: auto;
    padding: var(--space-6);
  }

  /* ── 更新橫幅 ── */
  .update-banner {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-5);
    background: var(--accent-muted);
    border-bottom: 1px solid color-mix(in srgb, var(--accent) 32%, transparent);
    font-size: 13px;
    flex-shrink: 0;
  }

  .banner-icon {
    width: 18px;
    height: 18px;
    color: var(--accent);
    flex-shrink: 0;
  }

  .update-banner span {
    flex: 1;
  }

  .banner-actions {
    display: flex;
    gap: var(--space-2);
    flex-shrink: 0;
  }

  @media (max-width: 999px) {
    .page { padding: var(--space-4); }
  }

  /* compact progress 模式：無側欄、內距收窄、禁止滾動（內容必須完整放得下） */
  .shell.compact .page { padding: var(--space-2); overflow: hidden; }
</style>
