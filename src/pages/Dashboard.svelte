<script lang="ts">
  import { onMount } from 'svelte';
  import { t } from 'svelte-i18n';
  import TopologyGrid from '../components/TopologyGrid.svelte';
  import AppliedTable from '../components/AppliedTable.svelte';
  import * as ipc from '../lib/ipc';
  import { topology, usage, rules, applied } from '../lib/stores';
  import { resolveCores } from '../lib/affinity';

  // 面板開啟時才串流使用率（PLAN §7.5 省電設計）
  onMount(() => {
    ipc.setUsageStreaming(true);
    return () => ipc.setUsageStreaming(false);
  });

  // 所有啟用中規則涵蓋的 LP 集合（面板高亮用）
  let covered = $derived.by(() => {
    const topo = $topology;
    if (!topo) return new Set<number>();
    const set = new Set<number>();
    for (const r of $rules) {
      if (!r.enabled) continue;
      for (const i of resolveCores(r.affinity, topo)) set.add(i);
    }
    return set;
  });
</script>

<h2>{$t('dashboard.cpuTitle')}</h2>
{#if $topology}
  <TopologyGrid topology={$topology} usage={$usage} {covered} />
  <div class="hint legend">{$t('dashboard.coveredLegend')}</div>
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
  .legend {
    margin-top: 8px;
  }
</style>
