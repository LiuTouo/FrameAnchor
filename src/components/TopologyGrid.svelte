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

<div class="grid panel">
  {#each topology.physicalCores as core (core.id)}
    <div class="core-row">
      <div class="core-label">
        <span>{$t('dashboard.coreLabel', { values: { id: core.id } })}</span>
        {#if topology.hasHybrid}
          <span class="badge" class:p-core={core.isPCore} class:e-core={!core.isPCore} aria-label={core.isPCore ? 'P-core' : 'E-core'}>
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
  }

  .core-row {
    display: flex;
    align-items: center;
    gap: var(--space-3);
  }

  .core-label {
    width: 90px;
    flex-shrink: 0;
    color: var(--text-secondary);
    font-size: 12px;
    display: flex;
    align-items: center;
    gap: 5px;
  }

  .badge.p-core {
    background: var(--accent-muted);
    color: var(--accent);
  }

  .badge.e-core {
    background: var(--surface-3);
    color: var(--text-secondary);
  }

  .lps {
    display: flex;
    gap: 6px;
    flex: 1;
  }
</style>
