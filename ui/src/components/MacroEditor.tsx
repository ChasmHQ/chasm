import { useState, useRef, useCallback, useEffect } from "react";
import { Plus, Trash2, Play, Square, CheckCircle2, XCircle, Loader2, SkipForward, Clock, Layers, Activity } from "lucide-react";
import { clsx } from "clsx";
import type { PublicClient, WalletClient, TestClient, Address, Hex } from "viem";
import type { Macro, MacroStep, MacroStepResult, MacroRunState } from "../types/macro";
import { MacroStep as MacroStepComponent } from "./MacroStep";
import { buildArgsFromInputs } from "../utils/abiUtils";
import Editor from "react-simple-code-editor";
import { highlight, languages } from "prismjs";
import "prismjs/components/prism-clike";

interface DeployedInstance {
  id: string;
  name: string;
  address: Address;
  artifact: { abi: any[]; bytecode: { object: string } };
  mode?: 'live' | 'local';
}

interface MacroEditorProps {
  macros: Macro[];
  activeMacroId: string | null;
  deployedInstances: DeployedInstance[];
  clients: { publicClient: PublicClient; walletClient?: WalletClient } | null;
  localClients: { publicClient: PublicClient; walletClient: WalletClient; testClient: TestClient; rpcUrl: string } | null;
  ensureLocalClients: () => Promise<{ publicClient: PublicClient; walletClient: WalletClient; testClient: TestClient; rpcUrl: string }>;
  globalMode: 'live' | 'local';
  rpcUrl: string;
  onLog: (msg: string) => void;
  onCreateMacro: (name: string) => void;
  onUpdateMacro: (updated: Macro) => void;
  onDeleteMacro: (id: string) => void;
  onSelectMacro: (id: string) => void;
}

interface TraceState {
  loading: boolean;
  data?: string;
  error?: string;
}

const stripAnsi = (str: string) => str.replace(/\x1B\[[0-9;]*[a-zA-Z]/g, "");

function newStep(): MacroStep {
  return {
    id: crypto.randomUUID(),
    name: 'New Step',
    enabled: true,
    contractInstanceId: '',
    functionName: '',
    params: {},
  };
}

function RunResultRow({
  result,
  label,
  trace,
  onTrace,
}: {
  result: MacroStepResult;
  label: string;
  trace?: TraceState;
  onTrace?: () => void;
}) {
  const [showTrace, setShowTrace] = useState(false);

  const iconMap = {
    pending: <Clock size={12} className="text-slate-600" />,
    running: <Loader2 size={12} className="text-indigo-400 animate-spin" />,
    success: <CheckCircle2 size={12} className="text-emerald-400" />,
    failed: <XCircle size={12} className="text-red-400" />,
    skipped: <SkipForward size={12} className="text-slate-500" />,
  };
  const colorMap = {
    pending: 'text-slate-500',
    running: 'text-indigo-300',
    success: 'text-emerald-300',
    failed: 'text-red-300',
    skipped: 'text-slate-500',
  };

  const canTrace = result.status === 'success' || result.status === 'failed';

  function handleTraceClick() {
    if (!trace?.data && onTrace) onTrace();
    setShowTrace((v) => !v);
  }

  return (
    <div className="border-b border-slate-800/50 last:border-0">
      <div className="flex items-start gap-2 py-1.5">
        <span className="shrink-0 mt-0.5">{iconMap[result.status]}</span>
        <div className="flex-1 min-w-0">
          <div className={clsx("text-xs font-medium", colorMap[result.status])}>{label}</div>
          {result.status === 'success' && result.txHash && (
            <div className="text-[10px] text-slate-500 font-mono truncate">
              tx: {result.txHash.slice(0, 14)}...
            </div>
          )}
          {result.status === 'success' && result.returnValue !== undefined && result.returnValue !== null && (
            <div className="text-[10px] text-slate-400 truncate">
              → {String(result.returnValue)}
            </div>
          )}
          {result.status === 'failed' && result.error && (
            <div className="text-[10px] text-red-500 break-all line-clamp-2" title={result.error}>
              {result.error}
            </div>
          )}
          {result.duration !== undefined && (
            <div className="text-[10px] text-slate-600">{result.duration}ms</div>
          )}
        </div>
        {canTrace && (
          <button
            onClick={handleTraceClick}
            title="Show trace"
            className={clsx(
              "shrink-0 p-0.5 rounded transition-colors",
              showTrace ? "text-indigo-400" : "text-slate-600 hover:text-indigo-400"
            )}
          >
            {trace?.loading
              ? <Loader2 size={11} className="animate-spin" />
              : <Activity size={11} />}
          </button>
        )}
      </div>

      {/* Trace panel */}
      {showTrace && (
        <div className="mb-2 rounded border border-slate-700/50 bg-slate-950/60 overflow-hidden">
          {trace?.loading && (
            <div className="flex items-center gap-2 p-2 text-[10px] text-slate-500">
              <Loader2 size={10} className="animate-spin" /> Loading trace...
            </div>
          )}
          {trace?.error && (
            <div className="p-2 text-[10px] text-red-400">{trace.error}</div>
          )}
          {trace?.data && (
            <Editor
              value={stripAnsi(trace.data)}
              onValueChange={() => {}}
              highlight={(code) => highlight(code, languages.clike, "clike")}
              padding={8}
              className="font-mono text-[10px] leading-relaxed"
              style={{ fontFamily: "monospace", backgroundColor: "transparent", color: "#e2e8f0" }}
              textareaClassName="focus:outline-none"
              readOnly
            />
          )}
          {!trace && (
            <div className="p-2 text-[10px] text-slate-500">Fetching trace...</div>
          )}
        </div>
      )}
    </div>
  );
}

export function MacroEditor({
  macros,
  activeMacroId,
  deployedInstances,
  clients,
  localClients,
  ensureLocalClients,
  globalMode,
  rpcUrl,
  onLog,
  onCreateMacro,
  onUpdateMacro,
  onDeleteMacro,
  onSelectMacro,
}: MacroEditorProps) {
  const [runState, setRunState] = useState<MacroRunState>({ isRunning: false, results: [] });
  const [runMode, setRunMode] = useState<'all' | 'selected'>('all');
  const [selectedStepIds, setSelectedStepIds] = useState<string[]>([]);
  const [traces, setTraces] = useState<Record<string, TraceState>>({});
  const [logPanelWidth, setLogPanelWidth] = useState(256);
  const abortRef = useRef(false);
  const dragRef = useRef<{ startX: number; startWidth: number } | null>(null);

  const macro = macros.find((m) => m.id === activeMacroId) ?? null;

  // Drag-to-resize logic for the Run Log panel
  useEffect(() => {
    function onMouseMove(e: MouseEvent) {
      if (!dragRef.current) return;
      const delta = dragRef.current.startX - e.clientX;
      const next = Math.max(180, Math.min(600, dragRef.current.startWidth + delta));
      setLogPanelWidth(next);
    }
    function onMouseUp() {
      dragRef.current = null;
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
    }
    document.addEventListener('mousemove', onMouseMove);
    document.addEventListener('mouseup', onMouseUp);
    return () => {
      document.removeEventListener('mousemove', onMouseMove);
      document.removeEventListener('mouseup', onMouseUp);
    };
  }, []);

  function startDrag(e: React.MouseEvent) {
    e.preventDefault();
    dragRef.current = { startX: e.clientX, startWidth: logPanelWidth };
    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';
  }

  // Fetch trace for a step result
  async function fetchTrace(result: MacroStepResult) {
    const stepId = result.stepId;
    setTraces((prev) => ({ ...prev, [stepId]: { loading: true } }));

    const effectiveRpcUrl = globalMode === 'local'
      ? (localClients?.rpcUrl ?? rpcUrl)
      : rpcUrl;

    try {
      if (result.txHash) {
        const res = await fetch(
          `http://localhost:3000/trace/${result.txHash}?rpc_url=${encodeURIComponent(effectiveRpcUrl)}`
        );
        const raw = await res.text();
        const data = JSON.parse(raw);
        if (data.error) throw new Error(data.error);
        setTraces((prev) => ({
          ...prev,
          [stepId]: { loading: false, data: data.stdout || data.stderr || 'No trace output.' },
        }));
      } else {
        // Fallback: calltree trace (for view or failed calls)
        const step = macro?.steps.find((s) => s.id === stepId);
        if (!step) throw new Error('Step not found');

        let toAddress: string;
        let callData: string;

        if ((step.inputMode ?? 'form') === 'raw') {
          toAddress = step.rawTo ?? '';
          callData = step.rawData?.trim() || '0x';
        } else {
          const instance = deployedInstances.find((i) => i.id === step.contractInstanceId);
          if (!instance) throw new Error('Instance not found');
          const abiItem = instance.artifact.abi.find(
            (item: any) => item.type === 'function' && item.name === step.functionName
          );
          if (!abiItem) throw new Error('ABI item not found');
          const args = buildArgsFromInputs(abiItem.inputs ?? [], step.params);
          const { encodeFunctionData } = await import('viem');
          callData = encodeFunctionData({ abi: [abiItem], functionName: step.functionName, args });
          toAddress = instance.address;
        }

        const call = {
          to: toAddress,
          data: callData,
          value: step.valueWei ? `0x${BigInt(step.valueWei).toString(16)}` : '0x0',
        };

        const res = await fetch('http://localhost:3000/trace/calltree', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ rpcUrl: effectiveRpcUrl, call, blockTag: 'latest' }),
        });
        const raw = await res.text();
        const parsed = JSON.parse(raw);
        if (parsed.error) throw new Error(parsed.error);
        setTraces((prev) => ({
          ...prev,
          [stepId]: { loading: false, data: parsed.stdout || parsed.stderr || 'No trace output.' },
        }));
      }
    } catch (e: any) {
      setTraces((prev) => ({
        ...prev,
        [stepId]: { loading: false, error: e?.message ?? String(e) },
      }));
    }
  }

  const updateResult = useCallback((stepId: string, patch: Partial<MacroStepResult>) => {
    setRunState((prev) => ({
      ...prev,
      results: prev.results.map((r) =>
        r.stepId === stepId ? { ...r, ...patch } : r
      ),
    }));
  }, []);

  async function executeStep(step: MacroStep): Promise<Partial<MacroStepResult>> {
    const valueWei = step.valueWei && step.valueWei !== '0' ? BigInt(step.valueWei) : undefined;
    const gas = step.gasOverride ? BigInt(step.gasOverride) : undefined;

    const lc = globalMode === 'local' ? await ensureLocalClients() : null;
    const pc = lc?.publicClient ?? clients?.publicClient;
    const wc: WalletClient | undefined = lc?.walletClient ?? clients?.walletClient;

    if (!pc) throw new Error("No public client available");

    // ── RAW MODE ──
    if ((step.inputMode ?? 'form') === 'raw') {
      const to = step.rawTo as Address | undefined;
      const data = (step.rawData?.trim() || '0x') as Hex;
      const isCall = step.rawMethod === 'call';

      if (isCall) {
        const res = await pc.call({ to, data, value: valueWei, gas });
        return { returnValue: res.data ?? '0x' };
      }

      if (!wc) throw new Error("No wallet client available");
      if (!wc.account) throw new Error("No account configured");

      const hash = await wc.sendTransaction({
        to,
        data,
        value: valueWei,
        gas,
        account: wc.account,
        chain: wc.chain,
      } as any);
      const receipt = await pc.waitForTransactionReceipt({ hash });
      return {
        txHash: hash,
        gasUsed: receipt.gasUsed?.toString(),
        blockNumber: Number(receipt.blockNumber),
      };
    }

    // ── FORM MODE ──
    const instance = deployedInstances.find((i) => i.id === step.contractInstanceId);
    if (!instance) throw new Error(`Instance not found: ${step.contractInstanceId}`);

    const abiItem = instance.artifact.abi.find(
      (item: any) => item.type === 'function' && item.name === step.functionName
    );
    if (!abiItem) throw new Error(`Function not found: ${step.functionName}`);

    const args = buildArgsFromInputs(abiItem.inputs ?? [], step.params);
    const isView =
      abiItem.stateMutability === 'view' || abiItem.stateMutability === 'pure';

    if (isView) {
      const res = await pc.readContract({
        address: instance.address,
        abi: [abiItem],
        functionName: step.functionName,
        args,
      });
      return { returnValue: res };
    }

    if (!wc) throw new Error("No wallet client available");

    const writeParams: any = {
      address: instance.address,
      abi: [abiItem],
      functionName: step.functionName,
      args,
    };
    if (valueWei !== undefined) writeParams.value = valueWei;
    if (gas !== undefined) writeParams.gas = gas;
    if (wc.account) writeParams.account = wc.account;

    const hash = await wc.writeContract(writeParams);
    const receipt = await pc.waitForTransactionReceipt({ hash });

    return {
      txHash: hash,
      gasUsed: receipt.gasUsed?.toString(),
      blockNumber: Number(receipt.blockNumber),
    };
  }

  async function runMacro(stepOrder: string[]) {
    if (!macro) return;
    abortRef.current = false;
    // Clear traces from previous run
    setTraces({});

    const initialResults: MacroStepResult[] = stepOrder.map((id) => ({
      stepId: id,
      status: 'pending',
    }));
    setRunState({ isRunning: true, results: initialResults });

    let preRunSnapshotId: string | undefined;
    if (macro.snapshotBeforeRun && globalMode === 'local') {
      try {
        const lc = await ensureLocalClients();
        preRunSnapshotId = await lc.testClient.snapshot();
        onLog(`[Macro: ${macro.name}] Snapshot taken before run`);
      } catch (e) {
        onLog(`[Macro: ${macro.name}] Failed to take snapshot: ${e}`);
      }
    }

    let hadFailure = false;

    for (const stepId of stepOrder) {
      if (abortRef.current) {
        onLog(`[Macro: ${macro.name}] Run cancelled`);
        break;
      }

      const step = macro.steps.find((s) => s.id === stepId);
      if (!step || !step.enabled) {
        updateResult(stepId, { status: 'skipped' });
        continue;
      }

      updateResult(stepId, { status: 'running' });
      const t0 = Date.now();
      onLog(`[Macro: ${macro.name}] Running: ${step.name}`);

      try {
        const result = await executeStep(step);
        const duration = Date.now() - t0;
        updateResult(stepId, { status: 'success', duration, ...result });
        onLog(`[Macro: ${macro.name}] ✓ ${step.name} (${duration}ms)`);
      } catch (e: any) {
        hadFailure = true;
        const duration = Date.now() - t0;
        const msg = e?.shortMessage ?? e?.message ?? String(e);
        updateResult(stepId, { status: 'failed', error: msg, duration });
        onLog(`[Macro: ${macro.name}] ✗ ${step.name}: ${msg}`);
        if (macro.revertOnFailure) break;
      }

      if (step.delayMs && step.delayMs > 0) {
        await new Promise((r) => setTimeout(r, step.delayMs));
      }
    }

    if (hadFailure && macro.revertOnFailure && preRunSnapshotId && globalMode === 'local') {
      try {
        const lc = await ensureLocalClients();
        await lc.testClient.revert({ id: preRunSnapshotId as Hex });
        onLog(`[Macro: ${macro.name}] Reverted to pre-run state`);
      } catch {
        onLog(`[Macro: ${macro.name}] Failed to revert snapshot`);
      }
    }

    setRunState((prev) => ({ ...prev, isRunning: false }));
  }

  function handleRunAll() {
    if (!macro) return;
    runMacro(macro.steps.filter((s) => s.enabled).map((s) => s.id));
  }

  function handleRunSelected() {
    if (selectedStepIds.length === 0) return;
    runMacro(selectedStepIds);
  }

  function handleCancel() {
    abortRef.current = true;
  }

  function updateStep(stepId: string, updated: MacroStep) {
    if (!macro) return;
    onUpdateMacro({ ...macro, steps: macro.steps.map((s) => (s.id === stepId ? updated : s)) });
  }

  function removeStep(stepId: string) {
    if (!macro) return;
    onUpdateMacro({ ...macro, steps: macro.steps.filter((s) => s.id !== stepId) });
    setSelectedStepIds((ids) => ids.filter((id) => id !== stepId));
  }

  function addStep() {
    if (!macro) return;
    onUpdateMacro({ ...macro, steps: [...macro.steps, newStep()] });
  }

  function duplicateStep(stepId: string) {
    if (!macro) return;
    const idx = macro.steps.findIndex((s) => s.id === stepId);
    if (idx === -1) return;
    const copy = { ...macro.steps[idx], id: crypto.randomUUID(), name: macro.steps[idx].name + ' (copy)' };
    const next = [...macro.steps];
    next.splice(idx + 1, 0, copy);
    onUpdateMacro({ ...macro, steps: next });
  }

  function moveStep(stepId: string, dir: -1 | 1) {
    if (!macro) return;
    const idx = macro.steps.findIndex((s) => s.id === stepId);
    if (idx === -1) return;
    const next = [...macro.steps];
    const swap = idx + dir;
    if (swap < 0 || swap >= next.length) return;
    [next[idx], next[swap]] = [next[swap], next[idx]];
    onUpdateMacro({ ...macro, steps: next });
  }

  function toggleStepSelected(stepId: string) {
    setSelectedStepIds((ids) =>
      ids.includes(stepId) ? ids.filter((id) => id !== stepId) : [...ids, stepId]
    );
  }

  function moveSelectedUp(stepId: string) {
    setSelectedStepIds((ids) => {
      const idx = ids.indexOf(stepId);
      if (idx <= 0) return ids;
      const next = [...ids];
      [next[idx - 1], next[idx]] = [next[idx], next[idx - 1]];
      return next;
    });
  }

  function moveSelectedDown(stepId: string) {
    setSelectedStepIds((ids) => {
      const idx = ids.indexOf(stepId);
      if (idx === -1 || idx >= ids.length - 1) return ids;
      const next = [...ids];
      [next[idx], next[idx + 1]] = [next[idx + 1], next[idx]];
      return next;
    });
  }

  return (
    <div className="flex h-full overflow-hidden text-sm">
      {/* Left: Macro List */}
      <div className="w-56 shrink-0 border-r border-slate-800 flex flex-col">
        <div className="p-3 border-b border-slate-800 flex items-center justify-between">
          <span className="text-xs font-bold text-slate-400 uppercase tracking-wide">Macros</span>
          <button
            onClick={() => onCreateMacro('New Macro')}
            className="p-1 rounded text-slate-500 hover:text-indigo-400 hover:bg-slate-800 transition-colors"
            title="New macro"
          >
            <Plus size={14} />
          </button>
        </div>
        <div className="flex-1 overflow-y-auto py-1">
          {macros.length === 0 && (
            <div className="px-4 py-6 text-xs text-slate-600 text-center">
              No macros yet.<br />Click + to create one.
            </div>
          )}
          {macros.map((m) => (
            <div
              key={m.id}
              onClick={() => onSelectMacro(m.id)}
              className={clsx(
                "flex items-center justify-between px-3 py-2 cursor-pointer group transition-colors",
                m.id === activeMacroId
                  ? "bg-indigo-950/40 text-indigo-300"
                  : "text-slate-400 hover:bg-slate-800/50 hover:text-slate-200"
              )}
            >
              <div className="flex items-center gap-2 min-w-0">
                <Layers size={12} className="shrink-0" />
                <span className="truncate text-xs">{m.name}</span>
                <span className="text-[10px] text-slate-600 shrink-0">{m.steps.length}</span>
              </div>
              <button
                onClick={(e) => { e.stopPropagation(); onDeleteMacro(m.id); }}
                className="opacity-0 group-hover:opacity-100 p-0.5 text-slate-600 hover:text-red-400 transition-all"
                title="Delete macro"
              >
                <Trash2 size={10} />
              </button>
            </div>
          ))}
        </div>
      </div>

      {/* Center: Step Editor */}
      <div className="flex-1 flex flex-col overflow-hidden min-w-0">
        {!macro ? (
          <div className="flex-1 flex items-center justify-center text-slate-600 text-sm">
            Select a macro or create one to get started.
          </div>
        ) : (
          <>
            {/* Toolbar */}
            <div className="border-b border-slate-800 px-4 py-2.5 flex items-center gap-3 shrink-0">
              <input
                className="bg-transparent text-slate-200 text-sm font-medium outline-none placeholder:text-slate-600 flex-1 min-w-0"
                value={macro.name}
                onChange={(e) => onUpdateMacro({ ...macro, name: e.target.value })}
                placeholder="Macro name..."
              />
              <label className="flex items-center gap-1.5 text-xs text-slate-500 cursor-pointer select-none">
                <input
                  type="checkbox"
                  className="accent-indigo-500"
                  checked={!!macro.snapshotBeforeRun}
                  onChange={(e) => onUpdateMacro({ ...macro, snapshotBeforeRun: e.target.checked })}
                />
                Snapshot before
              </label>
              <label className="flex items-center gap-1.5 text-xs text-slate-500 cursor-pointer select-none">
                <input
                  type="checkbox"
                  className="accent-red-500"
                  checked={!!macro.revertOnFailure}
                  onChange={(e) => onUpdateMacro({ ...macro, revertOnFailure: e.target.checked })}
                />
                Revert on fail
              </label>
              <div className="flex items-center gap-1 border border-slate-700 rounded overflow-hidden">
                <button
                  onClick={() => setRunMode('all')}
                  className={clsx(
                    "px-2 py-1 text-xs transition-colors",
                    runMode === 'all' ? "bg-slate-700 text-slate-200" : "text-slate-500 hover:text-slate-300"
                  )}
                >
                  All
                </button>
                <button
                  onClick={() => setRunMode('selected')}
                  className={clsx(
                    "px-2 py-1 text-xs transition-colors",
                    runMode === 'selected' ? "bg-slate-700 text-slate-200" : "text-slate-500 hover:text-slate-300"
                  )}
                >
                  Selected
                </button>
              </div>
              {runState.isRunning ? (
                <button
                  onClick={handleCancel}
                  className="flex items-center gap-1.5 px-3 py-1.5 rounded text-xs font-semibold bg-red-900/50 text-red-300 hover:bg-red-900 transition-colors"
                >
                  <Square size={12} /> Cancel
                </button>
              ) : (
                <button
                  onClick={runMode === 'all' ? handleRunAll : handleRunSelected}
                  disabled={runMode === 'selected' && selectedStepIds.length === 0}
                  className="flex items-center gap-1.5 px-3 py-1.5 rounded text-xs font-semibold bg-indigo-600 hover:bg-indigo-500 text-white disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
                >
                  <Play size={12} />
                  {runMode === 'all' ? 'Run All' : `Run (${selectedStepIds.length})`}
                </button>
              )}
            </div>

            {/* Steps */}
            <div className="flex-1 overflow-y-auto p-4 space-y-3">
              {macro.steps.length === 0 && (
                <div className="text-slate-600 text-xs text-center py-10">
                  No steps yet. Click "Add Step" below.
                </div>
              )}
              {macro.steps.map((step, idx) => {
                const result = runState.results.find((r) => r.stepId === step.id);
                return (
                  <div key={step.id} className="flex gap-2">
                    {runMode === 'selected' && (
                      <div className="flex flex-col items-center gap-1 pt-2 shrink-0">
                        <input
                          type="checkbox"
                          className="accent-indigo-500 w-3.5 h-3.5 cursor-pointer"
                          checked={selectedStepIds.includes(step.id)}
                          onChange={() => toggleStepSelected(step.id)}
                        />
                      </div>
                    )}
                    <div className="flex-1 min-w-0">
                      <MacroStepComponent
                        step={step}
                        stepNumber={idx + 1}
                        deployedInstances={deployedInstances}
                        result={result}
                        isRunning={runState.isRunning}
                        onUpdate={(updated) => updateStep(step.id, updated)}
                        onRemove={() => removeStep(step.id)}
                        onRunSingle={() => runMacro([step.id])}
                        onDuplicate={() => duplicateStep(step.id)}
                        onMoveUp={() => moveStep(step.id, -1)}
                        onMoveDown={() => moveStep(step.id, 1)}
                        isFirst={idx === 0}
                        isLast={idx === macro.steps.length - 1}
                      />
                    </div>
                  </div>
                );
              })}
              <button
                onClick={addStep}
                className="w-full py-2 border border-dashed border-slate-700 rounded text-xs text-slate-500 hover:text-slate-300 hover:border-slate-500 transition-colors flex items-center justify-center gap-1.5"
              >
                <Plus size={12} /> Add Step
              </button>
            </div>
          </>
        )}
      </div>

      {/* Drag handle */}
      <div
        onMouseDown={startDrag}
        className="w-1 shrink-0 cursor-col-resize bg-transparent hover:bg-indigo-500/40 transition-colors active:bg-indigo-500/60"
        title="Drag to resize"
      />

      {/* Right: Run Log */}
      <div
        className="shrink-0 border-l border-slate-800 flex flex-col overflow-hidden"
        style={{ width: logPanelWidth }}
      >
        <div className="p-3 border-b border-slate-800 flex items-center justify-between">
          <span className="text-xs font-bold text-slate-400 uppercase tracking-wide">Run Log</span>
          {runState.results.length > 0 && (
            <span className="text-[10px] text-slate-600">
              {runState.results.filter(r => r.status === 'success').length}/
              {runState.results.filter(r => r.status !== 'pending' && r.status !== 'skipped').length} ok
            </span>
          )}
        </div>

        {runMode === 'selected' && macro && selectedStepIds.length > 0 && (
          <div className="border-b border-slate-800 p-3 shrink-0">
            <div className="text-[10px] uppercase font-bold text-slate-500 mb-2">Execution Order</div>
            <div className="space-y-1">
              {selectedStepIds.map((stepId, pos) => {
                const step = macro.steps.find((s) => s.id === stepId);
                const stepNum = macro.steps.findIndex((s) => s.id === stepId) + 1;
                if (!step) return null;
                return (
                  <div key={stepId} className="flex items-center gap-1.5">
                    <span className="text-[10px] text-slate-600 w-4 shrink-0">{pos + 1}.</span>
                    <span className="text-xs text-slate-300 flex-1 truncate">
                      [{stepNum}] {step.name}
                    </span>
                    <button onClick={() => moveSelectedUp(stepId)} disabled={pos === 0} className="text-slate-600 hover:text-slate-400 disabled:opacity-20 text-[10px]">▲</button>
                    <button onClick={() => moveSelectedDown(stepId)} disabled={pos === selectedStepIds.length - 1} className="text-slate-600 hover:text-slate-400 disabled:opacity-20 text-[10px]">▼</button>
                  </div>
                );
              })}
            </div>
          </div>
        )}

        <div className="flex-1 overflow-y-auto p-3">
          {runState.results.length === 0 ? (
            <div className="text-xs text-slate-600 text-center py-6">No run yet.</div>
          ) : (
            <>
              {runState.results.map((result) => {
                const step = macro?.steps.find((s) => s.id === result.stepId);
                const stepNum = macro?.steps.findIndex((s) => s.id === result.stepId) ?? -1;
                return (
                  <RunResultRow
                    key={result.stepId}
                    result={result}
                    label={step ? `[${stepNum + 1}] ${step.name}` : result.stepId}
                    trace={traces[result.stepId]}
                    onTrace={() => fetchTrace(result)}
                  />
                );
              })}
            </>
          )}
        </div>
      </div>
    </div>
  );
}
