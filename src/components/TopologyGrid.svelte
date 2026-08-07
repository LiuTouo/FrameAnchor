<script lang="ts">
  import { t } from 'svelte-i18n';
  import CoreCell from '../components/CoreCell.svelte';
  import type { Topology } from '../lib/types';

  let {
    topology,
    usage,
    covered,
  }: {
    topology: Topology;
    usage: number[];
    covered: Set<number>;
  } = $props();

  const lpByIndex = (idx: number) => topology.logicalProcessors.find((lp) => lp.index === idx)!;
</script>

<div class="grid">
  {#each topology.physicalCores as core (core.id)}
    <div class="core-row">
      <div class="core-label">
        {$t('dashboard.coreLabel', { values: { id: core.id } })}
        {#if topology.hasHybrid}
          <span class="badge" class:p={core.isPCore} class:e={!core.isPCore}>
            {core.isPCore ? 'P' : 'E'}
          </span>
        {/if}
      </div>
      <div class="lps">
        {#each core.lpIndices as idx (idx)}
          <CoreCell
            lp={lpByIndex(idx)}
            usageValue={usage[idx] ?? null}
            covered={covered.has(idx)}
            showHt={topology.hasSmt}
          />
        {/each}
      </div>
    </div>
  {/each}
</div>

<style>
  .grid {
    display: flex;
    flex-direction: column;
    gap: 3px;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 10px;
  }
  .core-row {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .core-label {
    width: 90px;
    flex-shrink: 0;
    color: var(--muted);
    font-size: 12px;
    display: flex;
    align-items: center;
    gap: 5px;
  }
  .badge.p {
    background: rgba(79, 140, 255, 0.2);
    color: var(--accent);
  }
  .badge.e {
    background: rgba(139, 152, 171, 0.2);
    color: var(--muted);
  }
  .lps {
    display: flex;
    gap: 6px;
    flex: 1;
  }
</style>
