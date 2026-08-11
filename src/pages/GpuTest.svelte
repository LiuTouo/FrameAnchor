<script lang="ts">
  import { onMount } from 'svelte';
  import { t } from 'svelte-i18n';
  import * as ipc from '../lib/ipc';
  import {
    benchmarkProgress,
    benchmarkSessions,
    benchmarkState,
    gpuDevices,
    gpuPolicy,
    topology,
  } from '../lib/stores';
  import type {
    ApplyStatus,
    BenchmarkConfig,
    LpResult,
    SessionDetail,
    WorkloadKind,
  } from '../lib/types';
  import ConfirmDialog from '../components/ConfirmDialog.svelte';

  type ConfirmAction = 'start' | 'applyBest' | 'restore' | 'deleteHistory';

  // ── 表單狀態 ──
  let selectedGpu = $state('');
  let lps = $state<number[]>([]); // 選取候選 LP；空 = 全部
  let lpsInitialized = $state(false);
  let workload = $state<WorkloadKind>('Vulkan');
  let warmUpSecs = $state(5);
  let sampleSecs = $state(30);
  let repetitions = $state(1);
  let fullscreen = $state(true);
  let width = $state(640);
  let height = $state(480);
  let fpsCap = $state(0);
  let tripleBuffer = $state(false);

  let confirmAction = $state<ConfirmAction | null>(null);
  let deleteTargetId = $state<string | null>(null);
  let busy = $state(false);
  let errMsg = $state<string | null>(null);
  let detail = $state<SessionDetail | null>(null);
  let applyStatus = $state<ApplyStatus | null>(null);
  let policyLoading = $state(false);
  // 已處理過的終結 session（避免 reload 迴圈）
  let handledTerminal = $state<string | null>(null);

  const isRunning = $derived($benchmarkState?.status === 'Running');
  const recoveryRequired = $derived($benchmarkState?.recoveryRequired ?? false);
  const supportedLps = $derived(
    $topology ? $topology.logicalProcessors.map((p) => p.index).sort((a, b) => a - b) : [],
  );
  const lpList = $derived(lps.length ? lps : supportedLps);
  const restartCount = $derived(lpList.length * repetitions);
  const estMinutes = $derived(
    Math.max(1, Math.round((restartCount * (sampleSecs + warmUpSecs + 19)) / 60)),
  );
  // 策略面板操作的 GPU（檢視 session 時用該 session 的 GPU）
  const policyGpu = $derived(detail?.summary.gpuInstanceId || selectedGpu);
  const policyLp = $derived(
    $gpuPolicy ? maskToLp($gpuPolicy.assignmentSetOverride?.bytes ?? null) : null,
  );
  const policyDevicePolicy = $derived(
    $gpuPolicy ? bytesToU32($gpuPolicy.devicePolicy?.bytes ?? null) : null,
  );
  const results = $derived(detail?.results ?? []);

  function buildVulkanArgs(): string[] {
    // AutoGpuAffinity 內附 liblava workload 的格式：--fullscreen=<0|1> --width=<n>
    // --height=<n> --fps_cap=<n> --triple_buffering=<0|1>
    return [
      `--fullscreen=${fullscreen ? 1 : 0}`,
      `--width=${width}`,
      `--height=${height}`,
      `--fps_cap=${fpsCap}`,
      `--triple_buffering=${tripleBuffer ? 1 : 0}`,
    ];
  }

  // 預設：topology 載入後全部 LP 選取；第一張 GPU 自動選取
  $effect(() => {
    const topo = $topology;
    if (topo && !lpsInitialized) {
      lps = topo.logicalProcessors.map((p) => p.index).sort((a, b) => a - b);
      lpsInitialized = true;
    }
  });
  $effect(() => {
    if (selectedGpu) void refreshPolicyFor(selectedGpu);
  });
  // 終結 transition → 載入結果 + 刷新歷史
  $effect(() => {
    const st = $benchmarkState;
    if (!st) return;
    if (
      (st.status === 'Completed' || st.status === 'Failed' || st.status === 'Cancelled') &&
      st.sessionId &&
      st.sessionId !== handledTerminal
    ) {
      handledTerminal = st.sessionId;
      void loadDetail(st.sessionId).then(() => refreshHistory());
    }
  });

  onMount(() => {
    void init();
  });

  async function init() {
    try {
      gpuDevices.set(await ipc.enumerateGpus());
      benchmarkSessions.set(await ipc.listBenchmarkSessions());
      const st = await ipc.getBenchmarkState();
      benchmarkState.set(st);
      if (!selectedGpu && $gpuDevices.length) {
        selectedGpu = $gpuDevices[0].instanceId;
      }
      if (
        (st.status === 'Completed' || st.status === 'Failed' || st.status === 'Cancelled') &&
        st.sessionId
      ) {
        handledTerminal = st.sessionId;
        await loadDetail(st.sessionId);
      }
    } catch (e) {
      errMsg = String(e);
    }
  }

  async function refreshHistory() {
    benchmarkSessions.set(await ipc.listBenchmarkSessions());
  }

  async function loadDetail(id: string) {
    try {
      detail = await ipc.getBenchmarkSession(id);
      applyStatus = await ipc.getBenchmarkApplyStatus(id);
      if (detail) await refreshPolicyFor(detail.summary.gpuInstanceId || selectedGpu);
    } catch (e) {
      errMsg = String(e);
    }
  }

  async function refreshPolicyFor(instanceId: string) {
    if (!instanceId) return;
    policyLoading = true;
    try {
      gpuPolicy.set(await ipc.getGpuAffinityPolicy(instanceId));
    } catch {
      gpuPolicy.set(null);
    } finally {
      policyLoading = false;
    }
  }

  function toggleLp(i: number) {
    lps = lps.includes(i) ? lps.filter((x) => x !== i) : [...lps, i].sort((a, b) => a - b);
  }

  // ── 開始 / 取消 ──
  function startClicked() {
    errMsg = null;
    if (!selectedGpu) return void (errMsg = $t('gpuTest.errSelectGpu') as string);
    if (lpList.length === 0) return void (errMsg = $t('gpuTest.errSelectLp') as string);
    if (sampleSecs <= 0) return void (errMsg = $t('gpuTest.errSample') as string);
    if (repetitions < 1 || repetitions > 3)
      return void (errMsg = $t('gpuTest.errRepetitions') as string);
    if (width <= 0 || height <= 0) return void (errMsg = $t('gpuTest.errDimensions') as string);
    confirmAction = 'start';
  }

  async function doStart() {
    confirmAction = null;
    busy = true;
    try {
      const cfg: BenchmarkConfig = {
        candidateLps: lps,
        gpuInstanceId: selectedGpu,
        workload,
        warmUpSecs,
        sampleSecs,
        repetitions,
        syncWorkloadAffinity: false,
        fullscreen,
        width,
        height,
        fpsCap,
        tripleBuffer,
        vulkanArgs: buildVulkanArgs(),
        workloadExePath: null,
        presentmonPath: null,
        gamePath: null,
        windowTitle: null,
      };
      await ipc.startGpuBenchmark(cfg);
      errMsg = null;
    } catch (e) {
      errMsg = String(e);
    } finally {
      busy = false;
    }
  }

  async function doCancel() {
    busy = true;
    try {
      await ipc.cancelBenchmark();
    } catch (e) {
      errMsg = String(e);
    } finally {
      busy = false;
    }
  }

  // ── GPU 策略控制 ──
  async function applyBest() {
    if (!detail?.summary.bestLp) return;
    confirmAction = 'applyBest';
  }

  async function confirmApplyBest() {
    if (!detail?.summary.bestLp) return;
    confirmAction = null;
    busy = true;
    try {
      await ipc.applyBestGpuAffinity(detail.summary.id);
      errMsg = null;
      await refreshPolicyFor(detail!.summary.gpuInstanceId || selectedGpu);
    } catch (e) {
      errMsg = String(e);
    } finally {
      busy = false;
    }
  }

  async function restorePrevious() {
    confirmAction = 'restore';
  }

  async function confirmRestorePrevious() {
    confirmAction = null;
    busy = true;
    try {
      await ipc.restorePreviousGpuAffinity();
      errMsg = null;
      await refreshPolicyFor(policyGpu);
    } catch (e) {
      errMsg = String(e);
    } finally {
      busy = false;
    }
  }

  // ── 歷史 ──
  async function openHistory(id: string) {
    await loadDetail(id);
  }

  async function deleteHistory(id: string) {
    deleteTargetId = id;
    confirmAction = 'deleteHistory';
  }

  async function confirmDeleteHistory() {
    if (!deleteTargetId) return;
    const id = deleteTargetId;
    confirmAction = null;
    deleteTargetId = null;
    try {
      await ipc.deleteBenchmarkSession(id);
      if (detail?.summary.id === id) {
        detail = null;
        applyStatus = null;
      }
      await refreshHistory();
    } catch (e) {
      errMsg = String(e);
    }
  }

  // ── 顯示輔助 ──
  function errText(code: string | null): string {
    if (!code) return '';
    const key = `errors.${code}`;
    const localized = $t(key) as string;
    return localized === key ? code : localized;
  }

  function statusLabel(s: string): string {
    switch (s) {
      case 'Completed':
        return $t('gpuTest.statusCompleted') as string;
      case 'Cancelled':
        return $t('gpuTest.statusCancelled') as string;
      case 'Failed':
        return $t('gpuTest.statusFailed') as string;
      default:
        return s;
    }
  }

  // backend 的 BenchmarkStage enum：Init / Warmup / Collecting / Finalizing
  function stageLabel(stage: string | undefined): string {
    switch (stage) {
      case 'Init':
        return $t('gpuTest.stageStarting') as string;
      case 'Warmup':
        return $t('gpuTest.stageApplying') as string;
      case 'Collecting':
        return $t('gpuTest.stageCollecting') as string;
      case 'Finalizing':
        return $t('gpuTest.stageFinalizing') as string;
      default:
        return stage ?? '';
    }
  }

  function bytesToU32(bytes: number[] | null): number | null {
    if (!bytes || bytes.length !== 4) return null;
    return (bytes[0] | (bytes[1] << 8) | (bytes[2] << 16) | (bytes[3] << 24)) >>> 0;
  }

  function maskToLp(bytes: number[] | null): number | null {
    if (!bytes || bytes.length === 0) return null;
    let v = 0n;
    for (let i = 0; i < bytes.length; i++) v |= BigInt(bytes[i]) << BigInt(i * 8);
    let found: number | null = null;
    for (let i = 0; i < 64; i++) {
      if ((v & (1n << BigInt(i))) !== 0n) {
        if (found !== null) return null; // 多 bit → 非單一
        found = i;
      }
    }
    return found;
  }

  function fmtBytes(n: number): string {
    if (n >= 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
    if (n >= 1024) return `${(n / 1024).toFixed(1)} KB`;
    return `${n} B`;
  }

  // 每欄顯示 rank：保留第 1、2 個不同值（純顯示，不重算後端評分）
  function colBest(col: (r: LpResult) => number | null, higher: boolean) {
    const vals = results.map(col).filter((v): v is number => v != null);
    if (!vals.length) return { first: null as number | null, second: null as number | null };
    const sorted = [...vals].sort((a, b) => (higher ? b - a : a - b));
    const first = sorted[0];
    const second = sorted.find((v) => v !== first) ?? null;
    return { first, second };
  }

  const bestAvg = $derived(colBest((r) => r.avgFps, true));
  const bestMax = $derived(colBest((r) => r.maxFps, true));
  const bestMin = $derived(colBest((r) => r.minFps, true));
  const bestStdev = $derived(colBest((r) => r.stdevFps, false));
  const bestP1 = $derived(colBest((r) => r.p1Low, true));
  const bestP01 = $derived(colBest((r) => r.p01Low, true));
  const bestP001 = $derived(colBest((r) => r.p001Low, true));
  const bestP0005 = $derived(colBest((r) => r.p0005Low, true));

  function median(values: number[]): number | null {
    if (!values.length) return null;
    const sorted = [...values].sort((a, b) => a - b);
    const mid = Math.floor(sorted.length / 2);
    return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
  }

  const unusualThresholds = $derived.by(() => {
    const complete = results.filter(
      (r) =>
        r.completed &&
        r.avgFps != null &&
        r.p1Low != null &&
        r.p01Low != null &&
        r.stdevFps != null,
    );
    return {
      avgFps: median(complete.map((r) => r.avgFps as number)),
      p1Low: median(complete.map((r) => r.p1Low as number)),
      p01Low: median(complete.map((r) => r.p01Low as number)),
      stdevFps: median(complete.map((r) => r.stdevFps as number)),
    };
  });

  function isUnusual(r: LpResult, metric: 'avgFps' | 'p1Low' | 'p01Low' | 'stdevFps') {
    const value = r[metric];
    const threshold = unusualThresholds[metric];
    if (!r.completed || value == null || threshold == null) return false;
    if (metric === 'stdevFps') return threshold > 0 && value > threshold * 1.5;
    return value < threshold * 0.85;
  }
  const colMeta = $derived([
    { key: 'avgFps', label: 'colAvg', best: bestAvg },
    { key: 'maxFps', label: 'colMax', best: bestMax },
    { key: 'minFps', label: 'colMin', best: bestMin },
    { key: 'stdevFps', label: 'colStdev', best: bestStdev },
    { key: 'p1Low', label: 'colP1', best: bestP1 },
    { key: 'p01Low', label: 'colP01', best: bestP01 },
    { key: 'p001Low', label: 'colP001', best: bestP001 },
    { key: 'p0005Low', label: 'colP0005', best: bestP0005 },
  ]);

  function cellClass(
    v: number | null | undefined,
    best: { first: number | null; second: number | null },
    unusual = false,
  ) {
    if (v == null) return '';
    const rank = v === best.first ? 'best' : v === best.second ? 'second' : '';
    return unusual ? `${rank} unusual-value`.trim() : rank;
  }

  const progress = $derived($benchmarkProgress);
  const currentRound = $derived(progress?.round ?? null);
  const etaMin = $derived(progress?.etaSecs ? Math.max(1, Math.round(progress.etaSecs / 60)) : null);
  const canApply = $derived(applyStatus?.canApply ?? false);
</script>

<div class="gpu-test">
  {#if recoveryRequired}
    <div class="recovery-banner" role="alert">
      {$t('gpuTest.recoveryBanner')}
    </div>
  {/if}

  {#if isRunning}
    <!-- 執行中：鎖定頁面，只保留取消 -->
    <section class="panel running-panel" aria-live="polite">
      <h2>{$t('gpuTest.runningTitle')}</h2>
      <div class="active-warning">
        <strong>{$t('gpuTest.activeWarningZh')}</strong>
        <span class="hint">{$t('gpuTest.activeWarningEn')}</span>
      </div>
      <dl class="running-meta">
        <div>
          <dt>{$t('gpuTest.gpuSelect')}</dt>
          <dd>{$gpuDevices.find((d) => d.instanceId === selectedGpu)?.friendlyName ?? selectedGpu}</dd>
        </div>
        {#if currentRound != null}
          <div>
            <dt>{$t('gpuTest.repetitions')}</dt>
            <dd>{$t('gpuTest.round', { values: { round: currentRound + 1 } })}</dd>
          </div>
        {/if}
        <div>
          <dt>{$t('gpuTest.currentLp')}</dt>
          <dd>{$benchmarkState?.currentLp ?? '—'}</dd>
        </div>
        <div>
          <dt>{$t('gpuTest.progress')}</dt>
          <dd>{$benchmarkState?.progressPct ?? 0}%</dd>
        </div>
        {#if etaMin != null}
          <div>
            <dt>{$t('gpuTest.eta')}</dt>
            <dd>{$t('gpuTest.riskEstimate', { values: { minutes: etaMin } })}</dd>
          </div>
        {/if}
        <div>
          <dt>{$t('gpuTest.colStatus')}</dt>
          <dd>{stageLabel($benchmarkState?.stage)}</dd>
        </div>
      </dl>
      <div class="progress-track" aria-hidden="true">
        <div style="width: {($benchmarkState?.progressPct ?? 0)}%"></div>
      </div>
      <div class="toolbar">
        <button class="danger" disabled={busy} onclick={doCancel}>{$t('gpuTest.cancel')}</button>
      </div>
    </section>
  {:else}
    <!-- 設定表單 -->
    <section class="panel">
      <h2>{$t('nav.gpuTest')}</h2>
      <div class="form-grid">
        <label class="field">
          <span>{$t('gpuTest.gpuSelect')}</span>
          {#if $gpuDevices.length === 0}
            <span class="hint">{$t('gpuTest.noGpu')}</span>
          {:else}
            <select bind:value={selectedGpu}>
              {#each $gpuDevices as d (d.instanceId)}
                <option value={d.instanceId}>{d.friendlyName}</option>
              {/each}
            </select>
          {/if}
        </label>

        <label class="field">
          <span>{$t('gpuTest.workload')}</span>
          <select bind:value={workload}>
            <option value="Vulkan">Vulkan</option>
            <option value="D3D9">Direct3D 9</option>
          </select>
        </label>

        <label class="field">
          <span>{$t('gpuTest.warmup')}</span>
          <input type="number" bind:value={warmUpSecs} min="0" />
        </label>
        <label class="field">
          <span>{$t('gpuTest.sample')}</span>
          <input type="number" bind:value={sampleSecs} min="1" />
        </label>
        <label class="field">
          <span>{$t('gpuTest.repetitions')}</span>
          <input type="number" bind:value={repetitions} min="1" max="3" />
        </label>

        <div class="field">
          <span>{$t('gpuTest.lpSelect')}</span>
          <div class="lp-chips" role="group" aria-label={$t('gpuTest.lpSelect')}>
            <button
              class:selected={lps.length === supportedLps.length}
              onclick={() => (lps = [...supportedLps])}
              type="button"
            >
              {$t('gpuTest.allLps')}
            </button>
            {#each supportedLps as i (i)}
              <button
                class:selected={lps.includes(i)}
                onclick={() => toggleLp(i)}
                type="button"
                aria-pressed={lps.includes(i)}
              >
                {i}
              </button>
            {/each}
          </div>
        </div>

        {#if workload === 'Vulkan'}
          <div class="field vulkan-opts">
            <label class="check">
              <input type="checkbox" bind:checked={fullscreen} />
              <span>{$t('gpuTest.fullscreen')}</span>
            </label>
            <label class="sub">
              <span>{$t('gpuTest.width')}</span>
              <input type="number" bind:value={width} min="1" />
            </label>
            <label class="sub">
              <span>{$t('gpuTest.height')}</span>
              <input type="number" bind:value={height} min="1" />
            </label>
            <label class="sub">
              <span>{$t('gpuTest.fpsCap')}</span>
              <input type="number" bind:value={fpsCap} min="0" />
            </label>
            <label class="check">
              <input type="checkbox" bind:checked={tripleBuffer} />
              <span>{$t('gpuTest.tripleBuffer')}</span>
            </label>
          </div>
        {/if}
      </div>

      {#if errMsg}
        <div class="error" role="alert">{errText(errMsg)}</div>
      {/if}

      <div class="toolbar">
        <button
          class="primary"
          disabled={busy || recoveryRequired || $gpuDevices.length === 0}
          onclick={startClicked}
        >
          {$t('gpuTest.start')}
        </button>
      </div>
    </section>
  {/if}

  <!-- 結果 + 策略 -->
  {#if detail}
    <section class="panel">
      <div class="panel-head">
        <h2>{$t('gpuTest.resultsTitle')}</h2>
        <span class="badge status-{detail.summary.status.toLowerCase()}">
          {statusLabel(detail.summary.status)}
        </span>
        {#if detail.summary.status === 'Failed' && detail.summary.bestLp == null && detail.results.length > 0}
          <span class="badge warn">{$t('gpuTest.partialError')}</span>
        {/if}
      </div>

      {#if detail.summary.status === 'Failed' && detail.summary.error}
        <div class="error" role="alert">{errText(detail.summary.error)}</div>
      {/if}

      {#if detail.results.length > 0}
        <div class="table-scroll">
          <table class="metric-table">
            <thead>
              <tr>
                <th>{$t('gpuTest.colLp')}</th>
                {#each colMeta as c (c.key)}
                  <th>{$t(`gpuTest.${c.label}`)}</th>
                {/each}
                <th>{$t('gpuTest.colSamples')}</th>
              </tr>
            </thead>
            <tbody>
              {#each detail.results as r (r.lp)}
                <tr>
                  <td class="lp-cell">
                    {r.lp}
                    {#if r.lp === detail.summary.bestLp}
                      <span class="badge best">{$t('gpuTest.bestTag')}</span>
                    {/if}
                  </td>
                  <td class={cellClass(r.avgFps, bestAvg, isUnusual(r, 'avgFps'))}>{r.avgFps?.toFixed(1) ?? '—'}</td>
                  <td class={cellClass(r.maxFps, bestMax)}>{r.maxFps?.toFixed(1) ?? '—'}</td>
                  <td class={cellClass(r.minFps, bestMin)}>{r.minFps?.toFixed(1) ?? '—'}</td>
                  <td class={cellClass(r.stdevFps, bestStdev, isUnusual(r, 'stdevFps'))}>{r.stdevFps?.toFixed(1) ?? '—'}</td>
                  <td class={cellClass(r.p1Low, bestP1, isUnusual(r, 'p1Low'))}>{r.p1Low?.toFixed(1) ?? '—'}</td>
                  <td class={cellClass(r.p01Low, bestP01, isUnusual(r, 'p01Low'))}>{r.p01Low?.toFixed(1) ?? '—'}</td>
                  <td class={cellClass(r.p001Low, bestP001)}>{r.p001Low?.toFixed(1) ?? '—'}</td>
                  <td class={cellClass(r.p0005Low, bestP0005)}>{r.p0005Low?.toFixed(1) ?? '—'}</td>
                  <td>{r.sampleCount}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {:else}
        <div class="hint">{$t('gpuTest.emptyResults')}</div>
      {/if}

      <div class="meta-grid">
        <div class="meta-item">
          <span class="hint">{$t('gpuTest.metaGpu')}</span>
          <span>{detail.summary.gpuName || detail.summary.gpuInstanceId}</span>
        </div>
        <div class="meta-item">
          <span class="hint">{$t('gpuTest.metaCpuFp')}</span>
          <span class="mono" title={detail.summary.cpuFingerprint}>
            {detail.summary.cpuFingerprint.slice(0, 12)}…
          </span>
        </div>
        <div class="meta-item">
          <span class="hint">{$t('gpuTest.metaApi')}</span>
          <span>{detail.summary.config.workload}</span>
        </div>
        {#if detail.summary.bestLp != null}
          <div class="meta-item">
            <span class="hint">{$t('gpuTest.colBest')}</span>
            <span>{detail.summary.bestLp}</span>
          </div>
        {/if}
      </div>

      {#if detail.summary.status === 'Completed' && detail.summary.bestLp != null}
        <div class="policy-actions">
          <button
            class="primary"
            disabled={busy || !canApply || recoveryRequired}
            onclick={applyBest}
            title={canApply ? '' : (errText(applyStatus?.reason ?? null) || '')}
          >
            {$t('gpuTest.applyBest')}
          </button>
          {#if !canApply && applyStatus?.reason}
            <span class="hint apply-reason">{errText(applyStatus.reason)}</span>
          {/if}
          {#if policyLp != null && policyLp !== detail.summary.bestLp}
            <span class="hint mismatch">
              {$t('gpuTest.policyMismatch', {
                values: { current: policyLp, best: detail.summary.bestLp },
              })}
            </span>
          {/if}
        </div>
      {/if}
    </section>
  {/if}

  <!-- GPU 中斷親和性策略 -->
  <section class="panel">
    <div class="panel-head">
      <h2>{$t('gpuTest.policyTitle')}</h2>
      <span class="hint">{$t('gpuTest.policyCurrent')}</span>
    </div>
    {#if policyLoading}
      <span class="hint">{$t('gpuTest.policyLoading')}</span>
    {:else if $gpuPolicy}
      <dl class="policy-list">
        <div>
          <dt>{$t('gpuTest.policyDevicePolicy')}</dt>
          <dd>{policyDevicePolicy ?? '—'}</dd>
        </div>
        <div>
          <dt>{$t('gpuTest.policyOverride')}</dt>
          <dd class="mono">{$gpuPolicy.assignmentSetOverride?.bytes?.map((b) => b.toString(16).padStart(2, '0')).join(' ') ?? '—'}</dd>
        </div>
        <div>
          <dt>{$t('gpuTest.policyLp')}</dt>
          <dd>{policyLp != null ? policyLp : $t('gpuTest.policyNone')}</dd>
        </div>
      </dl>
    {:else}
      <span class="hint">{$t('gpuTest.policyNone')}</span>
    {/if}
    <div class="toolbar">
      <button class="primary" disabled={busy || recoveryRequired} onclick={restorePrevious}>
        {$t('gpuTest.restore')}
      </button>
    </div>
  </section>

  <!-- 歷史 -->
  <section class="panel">
    <div class="panel-head">
      <h2>{$t('gpuTest.historyTitle')}</h2>
      <span class="hint">
        {$t('gpuTest.storageInfo', {
          values: {
            bytes: fmtBytes($benchmarkSessions.reduce((a, s) => a + s.totalBytes, 0)),
            count: $benchmarkSessions.length,
          },
        })}
      </span>
    </div>
    {#if $benchmarkSessions.length === 0}
      <div class="hint">{$t('gpuTest.emptyHistory')}</div>
    {:else}
      <div class="table-scroll">
        <table class="history-table">
          <thead>
            <tr>
              <th>{$t('gpuTest.colDate')}</th>
              <th>{$t('gpuTest.colGpu')}</th>
              <th>{$t('gpuTest.cpuTag')}</th>
              <th>{$t('gpuTest.colApi')}</th>
              <th>{$t('gpuTest.colStatus')}</th>
              <th>{$t('gpuTest.colBest')}</th>
              <th>{$t('gpuTest.colBytes')}</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {#each $benchmarkSessions as s (s.id)}
              <tr class:active={detail?.summary.id === s.id}>
                <td>{s.startedAt.replace('T', ' ').slice(0, 19)}</td>
                <td>{s.gpuName || s.gpuInstanceId}</td>
                <td class="mono" title={s.cpuFingerprint}>{s.cpuFingerprint.slice(0, 8)}</td>
                <td>{s.config.workload}</td>
                <td>
                  <span
                    class="badge status-{s.status.toLowerCase()}"
                    title={s.status === 'Failed' && s.error ? errText(s.error) : ''}
                  >
                    {statusLabel(s.status)}
                  </span>
                </td>
                <td>{s.bestLp ?? '—'}</td>
                <td>{fmtBytes(s.totalBytes)}</td>
                <td class="row-actions">
                  <button onclick={() => openHistory(s.id)}>{$t('gpuTest.open')}</button>
                  <button class="danger" onclick={() => deleteHistory(s.id)}>{$t('gpuTest.delete')}</button>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </section>
</div>

<ConfirmDialog
  open={confirmAction === 'start'}
  title={$t('gpuTest.riskTitle') as string}
  message={$t('gpuTest.riskBody', {
    values: {
      gpu: $gpuDevices.find((d) => d.instanceId === selectedGpu)?.friendlyName ?? selectedGpu,
      count: restartCount,
    },
  }) as string}
  detail={$t('gpuTest.riskEstimate', { values: { minutes: estMinutes } }) as string}
  confirmLabel={$t('gpuTest.riskConfirm') as string}
  cancelLabel={$t('gpuTest.riskCancel') as string}
  {busy}
  onconfirm={doStart}
  oncancel={() => (confirmAction = null)}
/>

<ConfirmDialog
  open={confirmAction === 'applyBest'}
  title={$t('gpuTest.applyBestTitle') as string}
  message={$t('gpuTest.applyBestConfirm', {
    values: {
      lp: detail?.summary.bestLp ?? '',
      gpu: $gpuDevices.find((d) => d.instanceId === detail?.summary.gpuInstanceId)?.friendlyName ?? detail?.summary.gpuInstanceId ?? '',
    },
  }) as string}
  confirmLabel={$t('common.confirm') as string}
  cancelLabel={$t('common.cancel') as string}
  {busy}
  onconfirm={confirmApplyBest}
  oncancel={() => (confirmAction = null)}
/>

<ConfirmDialog
  open={confirmAction === 'restore'}
  title={$t('gpuTest.restoreTitle') as string}
  message={$t('gpuTest.restoreConfirm') as string}
  confirmLabel={$t('gpuTest.restore') as string}
  cancelLabel={$t('common.cancel') as string}
  {busy}
  onconfirm={confirmRestorePrevious}
  oncancel={() => (confirmAction = null)}
/>

<ConfirmDialog
  open={confirmAction === 'deleteHistory'}
  title={$t('gpuTest.deleteTitle') as string}
  message={$t('gpuTest.deleteConfirm') as string}
  confirmLabel={$t('gpuTest.delete') as string}
  cancelLabel={$t('common.cancel') as string}
  danger
  {busy}
  onconfirm={confirmDeleteHistory}
  oncancel={() => {
    confirmAction = null;
    deleteTargetId = null;
  }}
/>

<style>
  .gpu-test {
    display: flex;
    flex-direction: column;
    gap: 14px;
    max-width: 900px;
  }
  .recovery-banner {
    padding: 10px 14px;
    background: var(--panel);
    border: 1px solid var(--danger);
    border-radius: 8px;
    color: var(--danger);
    font-weight: 600;
  }
  .panel {
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 14px 16px;
  }
  .panel h2 {
    margin: 0 0 12px;
    font-size: 14px;
  }
  .panel-head {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-bottom: 10px;
  }
  .panel-head h2 {
    margin: 0;
    flex: 1;
  }
  .form-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(200px, 1fr));
    gap: 12px 16px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  .field > span {
    color: var(--muted);
    font-size: 12px;
  }
  .field.check {
    flex-direction: row;
    align-items: center;
    gap: 8px;
  }
  .vulkan-opts {
    grid-column: 1 / -1;
    display: flex;
    flex-wrap: wrap;
    gap: 10px 18px;
    align-items: flex-end;
  }
  .vulkan-opts .sub {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 12px;
    color: var(--muted);
  }
  .vulkan-opts .sub input {
    width: 90px;
  }
  .lp-chips {
    display: flex;
    flex-wrap: wrap;
    gap: 5px;
  }
  .lp-chips button {
    min-width: 32px;
    padding: 3px 8px;
    text-align: center;
  }
  .lp-chips button.selected {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }
  .toolbar {
    display: flex;
    gap: 8px;
    margin-top: 12px;
  }
  .toolbar.right {
    justify-content: flex-end;
  }
  .error {
    margin-top: 10px;
    color: var(--danger);
    font-size: 12px;
  }
  .running-panel {
    border-color: var(--accent);
  }
  .active-warning {
    background: var(--panel-2);
    border: 1px solid var(--accent);
    border-radius: 6px;
    padding: 12px;
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin-bottom: 12px;
  }
  .active-warning strong {
    color: #fff;
    font-size: 15px;
  }
  .running-meta {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(160px, 1fr));
    gap: 8px 16px;
    margin: 0 0 12px;
  }
  .running-meta dt {
    color: var(--muted);
    font-size: 12px;
  }
  .running-meta dd {
    margin: 0;
  }
  .progress-track {
    height: 8px;
    background: var(--panel-2);
    border-radius: 4px;
    overflow: hidden;
    margin-bottom: 4px;
  }
  .progress-track div {
    height: 100%;
    background: var(--accent);
    transition: width 0.3s;
  }
  .table-scroll {
    overflow-x: auto;
    margin-bottom: 10px;
  }
  table {
    border-collapse: collapse;
    width: 100%;
    font-variant-numeric: tabular-nums;
  }
  th,
  td {
    padding: 6px 10px;
    border-bottom: 1px solid var(--border);
    text-align: right;
    white-space: nowrap;
  }
  th:first-child,
  td:first-child {
    text-align: left;
  }
  th {
    color: var(--muted);
    font-size: 12px;
    font-weight: 600;
  }
  td.best {
    background: rgba(78, 140, 255, 0.18);
    color: #fff;
    font-weight: 700;
  }
  td.second {
    background: rgba(78, 140, 255, 0.09);
  }
  .lp-cell {
    font-weight: 600;
  }
  .badge.best {
    background: var(--accent);
    color: #fff;
  }
  td.unusual-value {
    color: #f0a33c;
  }
  .badge.warn {
    background: #f0a33c;
    color: #000;
  }
  .badge.status-completed {
    background: var(--ok);
    color: #000;
  }
  .badge.status-failed,
  .badge.status-cancelled {
    background: var(--danger);
    color: #fff;
  }
  .meta-grid {
    display: flex;
    flex-wrap: wrap;
    gap: 8px 20px;
    margin: 10px 0 4px;
  }
  .meta-item {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .mono {
    font-family: Consolas, monospace;
    font-size: 11px;
  }
  .policy-actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px;
    margin-top: 10px;
  }
  .apply-reason,
  .mismatch {
    max-width: 380px;
  }
  .mismatch {
    color: #f0a33c;
  }
  .policy-list {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin: 0 0 8px;
  }
  .policy-list div {
    display: flex;
    gap: 10px;
  }
  .policy-list dt {
    color: var(--muted);
    min-width: 110px;
  }
  .policy-list dd {
    margin: 0;
  }
  .history-table tr.active td {
    background: rgba(78, 140, 255, 0.08);
  }
  .row-actions {
    display: flex;
    gap: 6px;
    justify-content: flex-end;
  }
  button.danger {
    color: var(--danger);
    border-color: var(--danger);
  }
</style>
