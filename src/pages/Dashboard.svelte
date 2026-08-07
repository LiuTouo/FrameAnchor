<script lang="ts">
  import { onMount } from 'svelte';
  import { t } from 'svelte-i18n';
  import TopologyGrid from '../components/TopologyGrid.svelte';
  import AppliedTable from '../components/AppliedTable.svelte';
  import * as ipc from '../lib/ipc';
  import { topology, usage, applied } from '../lib/stores';
  import type { AppliedProcess } from '../lib/types';

  let selectedPid = $state<number | null>(null);

  let sortedApplied = $derived(
    [...$applied].sort((a, b) =>
      a.exeName.localeCompare(b.exeName) || a.pid - b.pid,
    ),
  );

  let selected = $derived(
    selectedPid != null
      ? sortedApplied.find((p) => p.pid === selectedPid) ?? null
      : null,
  );

  // 自動選擇第一個 PID；當前選擇消失時切至下一個；清單為空時清除
  $effect(() => {
    const list = sortedApplied;
    if (list.length === 0) {
      selectedPid = null;
      return;
    }
    if (selectedPid != null && list.some((p) => p.pid === selectedPid)) {
      return;
    }
    selectedPid = list[0].pid;
  });

  // usage streaming 僅在有 applied 程序時開啟（避免重複 invoke）
  let wasActive = $state(false);
  $effect(() => {
    const active = sortedApplied.length > 0;
    if (active !== wasActive) {
      wasActive = active;
      ipc.setUsageStreaming(active);
      if (!active) {
        usage.set([]);
      }
    }
  });

  // 面板卸載時關閉 streaming 並清除 usage
  onMount(() => {
    return () => {
      ipc.setUsageStreaming(false);
      usage.set([]);
    };
  });

  // covered = 所選程序目前套用的核心
  let covered = $derived(
    selected ? new Set(selected.currentCores) : new Set<number>(),
  );
</script>

<h2>{$t('dashboard.cpuTitle')}</h2>
{#if sortedApplied.length > 0 && $topology}
  <div class="selector-row">
    <select bind:value={selectedPid} aria-label={$t('dashboard.selectProcess')}>
      {#each sortedApplied as p (p.pid)}
        <option value={p.pid}>{p.exeName} ({p.pid}) — {p.ruleName}</option>
      {/each}
    </select>
    <span class="hint system-note">{$t('dashboard.systemPerCoreNote')}</span>
  </div>
  <TopologyGrid topology={$topology} usage={$usage} {covered} />
  <div class="hint legend">{$t('dashboard.coveredLegend')}</div>
{:else}
  <p class="hint empty-cpu">{$t('dashboard.emptyCpuPanel')}</p>
{/if}

<h2>{$t('dashboard.appliedTitle')}</h2>
<AppliedTable applied={$applied} />

<style>
  h2 {
    font-size: 14px;
    margin: 4px 0 10px;
  }
  h2:not(:first-child) {
    margin-top: 24px;
  }
  .selector-row {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-bottom: 10px;
    flex-wrap: wrap;
  }
  .selector-row select {
    background: var(--panel-2);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 4px 8px;
    font-size: 13px;
    max-width: 320px;
  }
  .system-note {
    font-size: 11px;
  }
  .legend {
    margin-top: 8px;
  }
  .empty-cpu {
    margin: 20px 0;
  }
</style>
