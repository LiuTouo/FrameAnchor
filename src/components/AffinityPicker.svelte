<script lang="ts">
  import { t } from 'svelte-i18n';
  import CoreCell from './CoreCell.svelte';
  import { resolveCores, detectMode } from '../lib/affinity';
  import type { AffinitySpec, AffinityMode, Recommendation, Topology } from '../lib/types';

  let {
    topology,
    spec,
    onchange,
    recommendation = null,
  }: {
    topology: Topology;
    spec: AffinitySpec;
    onchange: (spec: AffinitySpec) => void;
    recommendation?: Recommendation | null;
  } = $props();

  let resolved = $derived(resolveCores(spec, topology));

  function preset(mode: AffinityMode) {
    // Prefer 沒有核心時預設 LP 0，避免空清單
    let cores = spec.cores;
    if (mode === 'Prefer' && cores.length === 0) cores = [0];
    onchange({ mode, cores });
  }

  function toggle(index: number) {
    const next = new Set(resolved);
    if (next.has(index)) {
      next.delete(index);
    } else {
      next.add(index);
    }
    const cores = [...next].sort((a, b) => a - b);
    // 軟綁定模式下勾選維持 Prefer，不自動偵測回硬綁定 preset
    if (spec.mode === 'Prefer') {
      onchange({ mode: 'Prefer', cores });
      return;
    }
    // 手動勾選：若恰好等於 preset 就回歸該 preset，否則 Custom
    const mode = detectMode(next, topology);
    onchange({ mode, cores });
  }

  const lpByIndex = (idx: number) => topology.logicalProcessors.find((lp) => lp.index === idx)!;
</script>

{#if recommendation != null}
  <div class="rec-caption">
    <span class="hint">{$t('ruleImport.annotatedFrom')}</span>
    {#if recommendation?.adjusted}
      <span class="badge adjusted">{$t('ruleImport.adjusted')}</span>
    {/if}
  </div>
{/if}
<div class="presets">
  <button class:active={spec.mode === 'All'} onclick={() => preset('All')}>
    {$t('rules.presetAll')}
  </button>
  {#if topology.hasSmt}
    <button class:active={spec.mode === 'NoSmtSibling'} onclick={() => preset('NoSmtSibling')}>
      {$t('rules.presetNoSmt')}
    </button>
  {/if}
  {#if topology.hasHybrid}
    <button class:active={spec.mode === 'PCoresOnly'} onclick={() => preset('PCoresOnly')}>
      {$t('rules.presetPCores')}
    </button>
  {/if}
  <button class:active={spec.mode === 'Custom'} disabled title={$t('rules.presetCustom')}>
    {$t('rules.presetCustom')}
  </button>
  <button class:active={spec.mode === 'Prefer'} onclick={() => preset('Prefer')}>
    {$t('rules.presetPrefer')}
  </button>
</div>

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
            interactive
            checked={resolved.has(idx)}
            showHt={topology.hasSmt}
            ontoggle={toggle}
          />
        {/each}
      </div>
    </div>
  {/each}
</div>

<style>
  .rec-caption {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 6px;
  }
  .badge.adjusted {
    background: rgba(240, 163, 60, 0.2);
    color: #f0a33c;
  }
  .presets {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-bottom: 8px;
  }
  .presets button.active {
    border-color: var(--accent);
    color: var(--accent);
  }
  .presets button:disabled {
    opacity: 0.8;
  }
  .grid {
    display: flex;
    flex-direction: column;
    gap: 3px;
    padding: 2px;
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
    flex-wrap: wrap;
  }
</style>
