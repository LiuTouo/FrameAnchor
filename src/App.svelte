<script lang="ts">
  import { onMount } from 'svelte';
  import { listen } from '@tauri-apps/api/event';
  import { locale, t } from 'svelte-i18n';
  import Dashboard from './pages/Dashboard.svelte';
  import Rules from './pages/Rules.svelte';
  import SettingsPage from './pages/Settings.svelte';
  import * as ipc from './lib/ipc';
  import { topology, rules, settings, applied, usage } from './lib/stores';
  import type { AppliedProcess } from './lib/types';

  type Tab = 'dashboard' | 'rules' | 'settings';
  let tab = $state<Tab>('dashboard');

  onMount(() => {
    let unlisteners: Array<() => void> = [];
    (async () => {
      topology.set(await ipc.getTopology());
      rules.set(await ipc.getRules());
      const s = await ipc.getSettings();
      settings.set(s);
      locale.set(s.language);
      applied.set(await ipc.getApplied());
      unlisteners.push(await listen<number[]>('usage-update', (e) => usage.set(e.payload)));
      unlisteners.push(
        await listen<AppliedProcess[]>('applied-update', (e) => applied.set(e.payload)),
      );
    })();
    return () => unlisteners.forEach((u) => u());
  });
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
</style>
