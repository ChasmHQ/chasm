import { useState, useRef, useEffect } from "react";
import { Play, ChevronDown, ChevronUp, Trash2, Copy, CheckCircle2, XCircle, Loader2, Clock, SkipForward, ChevronRight } from "lucide-react";
import { clsx } from "clsx";
import type { Address } from "viem";
import { parseUnits, formatUnits } from "viem";
import type { MacroStep as MacroStepType, MacroStepResult } from "../types/macro";
import { ParamInputs } from "./ParamInputs";

type ValueUnit = 'ether' | 'gwei' | 'wei';

function unitDecimals(u: ValueUnit) {
  return u === 'wei' ? 0 : u === 'gwei' ? 9 : 18;
}

function ValueInput({
  valueWei,
  valueUnit,
  onChange,
}: {
  valueWei?: string;
  valueUnit?: ValueUnit;
  onChange: (wei: string, unit: ValueUnit) => void;
}) {
  const initUnit: ValueUnit = (valueUnit as ValueUnit) ?? 'ether';

  // Derive display value from stored wei + unit
  function weiToDisplay(wei: string | undefined, u: ValueUnit): string {
    if (!wei || wei === '0' || wei === '') return '';
    try {
      return formatUnits(BigInt(wei), unitDecimals(u));
    } catch {
      return '';
    }
  }

  const [unit, setUnit] = useState<ValueUnit>(initUnit);
  const [display, setDisplay] = useState(() => weiToDisplay(valueWei, initUnit));
  const skipNextEffect = useRef(false);

  // Sync display when valueWei changes from outside (e.g. duplicate step)
  useEffect(() => {
    if (skipNextEffect.current) { skipNextEffect.current = false; return; }
    setDisplay(weiToDisplay(valueWei, unit));
  }, [valueWei]); // eslint-disable-line react-hooks/exhaustive-deps

  const [dropdownOpen, setDropdownOpen] = useState(false);
  const dropRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    function onOutside(e: MouseEvent) {
      if (dropRef.current && !dropRef.current.contains(e.target as Node)) setDropdownOpen(false);
    }
    document.addEventListener('mousedown', onOutside);
    return () => document.removeEventListener('mousedown', onOutside);
  }, []);

  function handleDisplayChange(val: string) {
    setDisplay(val);
    skipNextEffect.current = true;
    if (!val) { onChange('', unit); return; }
    try {
      const wei = parseUnits(val, unitDecimals(unit));
      onChange(wei.toString(), unit);
    } catch {
      onChange('', unit);
    }
  }

  function handleUnitChange(newUnit: ValueUnit) {
    setDropdownOpen(false);
    if (!display) { setUnit(newUnit); onChange('', newUnit); return; }
    try {
      const wei = parseUnits(display, unitDecimals(unit));
      const newDisplay = formatUnits(wei, unitDecimals(newUnit));
      setUnit(newUnit);
      setDisplay(newDisplay);
      skipNextEffect.current = true;
      onChange(wei.toString(), newUnit);
    } catch {
      setUnit(newUnit);
      onChange('', newUnit);
    }
  }

  return (
    <div className="flex relative" ref={dropRef}>
      <input
        className="flex-1 min-w-0 bg-transparent border-b border-slate-700 py-1.5 px-1 text-sm text-slate-200 focus:border-indigo-500 outline-none placeholder:text-slate-700"
        placeholder="0.0"
        type="number"
        value={display}
        onChange={(e) => handleDisplayChange(e.target.value)}
      />
      <button
        onClick={() => setDropdownOpen((o) => !o)}
        className="border-b border-slate-700 bg-transparent text-[10px] font-bold text-slate-400 hover:text-slate-200 hover:bg-slate-800 px-2 flex items-center gap-1 transition-colors whitespace-nowrap"
      >
        {unit === 'ether' ? 'ETH' : unit.toUpperCase()}
        <ChevronDown size={9} />
      </button>
      {dropdownOpen && (
        <div className="absolute top-full right-0 mt-1 bg-slate-900 border border-slate-700 rounded shadow-lg z-50 w-20 overflow-hidden">
          {(['ether', 'gwei', 'wei'] as ValueUnit[]).map((u) => (
            <div
              key={u}
              onClick={() => handleUnitChange(u)}
              className={clsx(
                'px-3 py-1.5 text-xs cursor-pointer hover:bg-slate-800 transition-colors uppercase',
                unit === u ? 'text-indigo-400 font-bold bg-slate-800/50' : 'text-slate-400'
              )}
            >
              {u === 'ether' ? 'ETH' : u}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

interface DeployedInstance {
  id: string;
  name: string;
  address: Address;
  artifact: { abi: any[]; bytecode: { object: string } };
  mode?: 'live' | 'local';
}

interface MacroStepProps {
  step: MacroStepType;
  stepNumber: number;
  deployedInstances: DeployedInstance[];
  result?: MacroStepResult;
  isRunning: boolean;
  onUpdate: (step: MacroStepType) => void;
  onRemove: () => void;
  onRunSingle: () => void;
  onDuplicate: () => void;
  onMoveUp: () => void;
  onMoveDown: () => void;
  isFirst: boolean;
  isLast: boolean;
}

function StatusBadge({ result }: { result?: MacroStepResult }) {
  if (!result || result.status === 'pending') return null;

  if (result.status === 'running') {
    return (
      <div className="flex items-center gap-1.5 text-indigo-400 text-xs">
        <Loader2 size={12} className="animate-spin" />
        <span>Running...</span>
      </div>
    );
  }
  if (result.status === 'success') {
    return (
      <div className="flex items-center gap-2 text-xs">
        <CheckCircle2 size={12} className="text-emerald-400 shrink-0" />
        <span className="text-emerald-400">Success</span>
        {result.txHash && (
          <span className="text-slate-500 font-mono">{result.txHash.slice(0, 10)}...</span>
        )}
        {result.gasUsed && (
          <span className="text-slate-500">Gas: {Number(result.gasUsed).toLocaleString()}</span>
        )}
        {result.duration !== undefined && (
          <span className="text-slate-600">{result.duration}ms</span>
        )}
        {result.returnValue !== undefined && result.returnValue !== null && (
          <span className="text-slate-400 truncate max-w-[200px]">
            → {String(result.returnValue)}
          </span>
        )}
      </div>
    );
  }
  if (result.status === 'failed') {
    return (
      <div className="flex items-center gap-1.5 text-xs">
        <XCircle size={12} className="text-red-400 shrink-0" />
        <span className="text-red-400">Failed</span>
        {result.error && (
          <span className="text-slate-500 truncate max-w-[300px]" title={result.error}>
            {result.error}
          </span>
        )}
      </div>
    );
  }
  if (result.status === 'skipped') {
    return (
      <div className="flex items-center gap-1.5 text-slate-500 text-xs">
        <SkipForward size={12} />
        <span>Skipped</span>
      </div>
    );
  }
  return null;
}

export function MacroStep({
  step,
  stepNumber,
  deployedInstances,
  result,
  isRunning,
  onUpdate,
  onRemove,
  onRunSingle,
  onDuplicate,
  onMoveUp,
  onMoveDown,
  isFirst,
  isLast,
}: MacroStepProps) {
  const [expanded, setExpanded] = useState(true);

  const inputMode = step.inputMode ?? 'form';

  const instance = deployedInstances.find((i) => i.id === step.contractInstanceId);
  const functions = instance
    ? instance.artifact.abi.filter((item: any) => item.type === 'function')
    : [];
  const selectedFn = functions.find((fn: any) => fn.name === step.functionName);
  const isView =
    selectedFn?.stateMutability === 'view' || selectedFn?.stateMutability === 'pure';

  function handleInstanceChange(instanceId: string) {
    const newInst = deployedInstances.find((i) => i.id === instanceId);
    const firstFn = newInst
      ? newInst.artifact.abi.find((item: any) => item.type === 'function')
      : null;
    onUpdate({ ...step, contractInstanceId: instanceId, functionName: firstFn?.name ?? '', params: {} });
  }

  function handleFunctionChange(fnName: string) {
    onUpdate({ ...step, functionName: fnName, params: {} });
  }

  function switchMode(mode: 'form' | 'raw') {
    // When switching to raw and a contract is selected, pre-fill `to`
    if (mode === 'raw' && instance && !step.rawTo) {
      onUpdate({ ...step, inputMode: mode, rawTo: instance.address, rawMethod: step.rawMethod ?? 'send' });
    } else {
      onUpdate({ ...step, inputMode: mode, rawMethod: step.rawMethod ?? 'send' });
    }
  }

  return (
    <div
      className={clsx(
        "rounded-lg border transition-all",
        result?.status === 'running'
          ? "border-indigo-500/60 bg-indigo-950/20"
          : result?.status === 'success'
          ? "border-emerald-800/40 bg-emerald-950/10"
          : result?.status === 'failed'
          ? "border-red-800/40 bg-red-950/10"
          : "border-slate-700/50 bg-slate-900/40"
      )}
    >
      {/* Header */}
      <div className="flex items-center gap-2 px-3 py-2">
        <span className="text-xs font-mono text-slate-600 w-5 shrink-0">{stepNumber}</span>

        {/* Enable toggle */}
        <button
          onClick={() => onUpdate({ ...step, enabled: !step.enabled })}
          className={clsx(
            "w-3 h-3 rounded-full border shrink-0 transition-colors",
            step.enabled ? "bg-indigo-500 border-indigo-500" : "bg-transparent border-slate-600"
          )}
          title={step.enabled ? "Disable step" : "Enable step"}
        />

        {/* Name input */}
        <input
          className="flex-1 bg-transparent text-sm text-slate-200 placeholder:text-slate-600 outline-none min-w-0"
          placeholder="Step name..."
          value={step.name}
          onChange={(e) => onUpdate({ ...step, name: e.target.value })}
        />

        {/* Result inline */}
        <StatusBadge result={result} />

        {/* Actions */}
        <div className="flex items-center gap-1 shrink-0">
          <button
            onClick={onRunSingle}
            disabled={isRunning || !step.enabled}
            title="Run this step"
            className="p-1 text-slate-500 hover:text-indigo-400 disabled:opacity-30 transition-colors"
          >
            <Play size={12} />
          </button>
          <button onClick={onMoveUp} disabled={isFirst} className="p-1 text-slate-600 hover:text-slate-400 disabled:opacity-20 transition-colors">
            <ChevronUp size={12} />
          </button>
          <button onClick={onMoveDown} disabled={isLast} className="p-1 text-slate-600 hover:text-slate-400 disabled:opacity-20 transition-colors">
            <ChevronDown size={12} />
          </button>
          <button onClick={onDuplicate} className="p-1 text-slate-600 hover:text-slate-400 transition-colors" title="Duplicate step">
            <Copy size={12} />
          </button>
          <button onClick={onRemove} className="p-1 text-slate-600 hover:text-red-400 transition-colors" title="Remove step">
            <Trash2 size={12} />
          </button>
          <button onClick={() => setExpanded((e) => !e)} className="p-1 text-slate-600 hover:text-slate-400 transition-colors">
            {expanded ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
          </button>
        </div>
      </div>

      {/* Body */}
      {expanded && (
        <div className="px-4 pb-4 pt-2 space-y-4 border-t border-slate-700/30">

          {/* Mode toggle */}
          <div className="flex items-center gap-2">
            <div className="flex items-center border border-slate-700 rounded overflow-hidden">
              <button
                onClick={() => switchMode('form')}
                className={clsx(
                  "px-2.5 py-1 text-[10px] font-bold uppercase transition-colors",
                  inputMode === 'form'
                    ? "bg-indigo-600/30 text-indigo-300 border-r border-slate-700"
                    : "text-slate-500 hover:text-slate-300 border-r border-slate-700"
                )}
              >
                Form
              </button>
              <button
                onClick={() => switchMode('raw')}
                className={clsx(
                  "px-2.5 py-1 text-[10px] font-bold uppercase transition-colors",
                  inputMode === 'raw'
                    ? "bg-orange-600/20 text-orange-300"
                    : "text-slate-500 hover:text-slate-300"
                )}
              >
                Raw
              </button>
            </div>
            {inputMode === 'raw' && (
              <span className="text-[10px] text-slate-500">Custom calldata — no ABI required</span>
            )}
          </div>

          {/* ── FORM MODE ── */}
          {inputMode === 'form' && (
            <>
              <div className="grid grid-cols-2 gap-3">
                <div className="flex flex-col gap-1">
                  <label className="text-[10px] uppercase font-bold text-slate-500">Contract Instance</label>
                  <select
                    className="bg-slate-800 border border-slate-700 rounded px-2 py-1.5 text-sm text-slate-200 outline-none focus:border-indigo-500"
                    value={step.contractInstanceId}
                    onChange={(e) => handleInstanceChange(e.target.value)}
                  >
                    <option value="">— select instance —</option>
                    {deployedInstances.map((inst) => (
                      <option key={inst.id} value={inst.id}>
                        {inst.name} ({inst.address.slice(0, 6)}...{inst.address.slice(-4)})
                      </option>
                    ))}
                  </select>
                </div>
                <div className="flex flex-col gap-1">
                  <label className="text-[10px] uppercase font-bold text-slate-500">
                    Function
                    {isView && <span className="ml-1 text-sky-600">view</span>}
                  </label>
                  <select
                    className="bg-slate-800 border border-slate-700 rounded px-2 py-1.5 text-sm text-slate-200 outline-none focus:border-indigo-500"
                    value={step.functionName}
                    onChange={(e) => handleFunctionChange(e.target.value)}
                    disabled={!instance}
                  >
                    <option value="">— select function —</option>
                    {functions.map((fn: any) => (
                      <option key={fn.name} value={fn.name}>
                        {fn.name}{fn.stateMutability === 'view' || fn.stateMutability === 'pure' ? ' (view)' : ''}
                      </option>
                    ))}
                  </select>
                </div>
              </div>

              {selectedFn && (
                <ParamInputs
                  abiInputs={selectedFn.inputs ?? []}
                  values={step.params}
                  onChange={(next) => onUpdate({ ...step, params: next })}
                />
              )}
            </>
          )}

          {/* ── RAW MODE ── */}
          {inputMode === 'raw' && (
            <div className="space-y-3">
              {/* Method */}
              <div className="flex items-center gap-2">
                <label className="text-[10px] uppercase font-bold text-slate-500 w-20 shrink-0">Method</label>
                <div className="flex items-center border border-slate-700 rounded overflow-hidden">
                  <button
                    onClick={() => onUpdate({ ...step, rawMethod: 'send' })}
                    className={clsx(
                      "px-3 py-1 text-xs transition-colors border-r border-slate-700",
                      (step.rawMethod ?? 'send') === 'send'
                        ? "bg-slate-700 text-slate-200"
                        : "text-slate-500 hover:text-slate-300"
                    )}
                  >
                    eth_sendTransaction
                  </button>
                  <button
                    onClick={() => onUpdate({ ...step, rawMethod: 'call' })}
                    className={clsx(
                      "px-3 py-1 text-xs transition-colors",
                      step.rawMethod === 'call'
                        ? "bg-slate-700 text-slate-200"
                        : "text-slate-500 hover:text-slate-300"
                    )}
                  >
                    eth_call
                  </button>
                </div>
              </div>

              {/* To */}
              <div className="flex items-center gap-2">
                <label className="text-[10px] uppercase font-bold text-slate-500 w-20 shrink-0">To</label>
                <div className="flex-1 relative">
                  <input
                    className="w-full bg-transparent border-b border-slate-700 py-1.5 px-1 text-sm text-slate-200 font-mono focus:border-indigo-500 outline-none placeholder:text-slate-700"
                    placeholder="0x... (contract address)"
                    value={step.rawTo ?? ''}
                    onChange={(e) => onUpdate({ ...step, rawTo: e.target.value })}
                  />
                  {deployedInstances.length > 0 && (
                    <select
                      className="absolute right-0 top-0 bg-slate-800 border-0 text-[10px] text-slate-500 outline-none cursor-pointer h-full px-1"
                      value=""
                      onChange={(e) => {
                        const inst = deployedInstances.find((i) => i.id === e.target.value);
                        if (inst) onUpdate({ ...step, rawTo: inst.address });
                      }}
                      title="Pick from deployed instances"
                    >
                      <option value="">pick</option>
                      {deployedInstances.map((inst) => (
                        <option key={inst.id} value={inst.id}>
                          {inst.name} ({inst.address.slice(0, 6)}...)
                        </option>
                      ))}
                    </select>
                  )}
                </div>
              </div>

              {/* Data */}
              <div className="flex flex-col gap-1.5">
                <label className="text-[10px] uppercase font-bold text-slate-500">
                  Call Data (hex)
                </label>
                <textarea
                  className="w-full bg-slate-900/60 border border-slate-700 rounded px-3 py-2 text-xs text-slate-200 font-mono focus:border-indigo-500 outline-none placeholder:text-slate-700 resize-none"
                  rows={4}
                  placeholder={"0x... (ABI-encoded calldata)\ne.g. 0x2e1a7d4d0000000000000000000000000000000000000000000000000de0b6b3a7640000"}
                  value={step.rawData ?? ''}
                  onChange={(e) => onUpdate({ ...step, rawData: e.target.value })}
                  spellCheck={false}
                />
                <p className="text-[10px] text-slate-600">
                  Tip: use cast calldata &lt;sig&gt; &lt;args&gt; to encode, or leave empty for a plain ETH transfer.
                </p>
              </div>
            </div>
          )}

          {/* Value / Gas / Delay — shared for both modes */}
          <div className="grid grid-cols-3 gap-3 pt-1">
            <div className="flex flex-col gap-1">
              <label className="text-[10px] uppercase font-bold text-slate-500">Value</label>
              <ValueInput
                valueWei={step.valueWei}
                valueUnit={(step.valueUnit as ValueUnit) ?? 'ether'}
                onChange={(wei, unit) => onUpdate({ ...step, valueWei: wei, valueUnit: unit })}
              />
            </div>
            <div className="flex flex-col gap-1">
              <label className="text-[10px] uppercase font-bold text-slate-500">Gas Limit</label>
              <input
                className="bg-transparent border-b border-slate-700 py-1.5 px-1 text-sm text-slate-200 focus:border-indigo-500 outline-none placeholder:text-slate-700"
                placeholder="auto"
                value={step.gasOverride ?? ''}
                onChange={(e) => onUpdate({ ...step, gasOverride: e.target.value })}
              />
            </div>
            <div className="flex flex-col gap-1">
              <label className="text-[10px] uppercase font-bold text-slate-500 flex items-center gap-1">
                <Clock size={10} /> Delay (ms)
              </label>
              <input
                className="bg-transparent border-b border-slate-700 py-1.5 px-1 text-sm text-slate-200 focus:border-indigo-500 outline-none placeholder:text-slate-700"
                placeholder="0"
                type="number"
                min="0"
                value={step.delayMs ?? ''}
                onChange={(e) =>
                  onUpdate({ ...step, delayMs: e.target.value ? Number(e.target.value) : undefined })
                }
              />
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
