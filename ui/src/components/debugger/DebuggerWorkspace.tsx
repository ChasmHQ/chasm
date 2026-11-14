import { useLayoutEffect, useRef, useState } from 'react'
import { ArrowDownToLine, Bug, Download, Play, RotateCcw, SkipForward, Square, StepForward, Terminal, X } from 'lucide-react'
import type { DebuggerSession } from '../../hooks/useDebuggerSession'
import type { DebuggerLaunchIntent, DebuggerMethod, DebugRecord, DebugValue, MutationTarget, NavigateAction } from '../../types/debugger'
import { display, record, rows, safeStringify } from '../../types/debugger'
import { DebuggerLaunch, buttonClass, inputClass } from './DebuggerLaunch'
import { DataTable, DebuggerPanels, Empty, KeyValues, StructuredValue } from './DebuggerPanels'

export function DebuggerWorkspace({ debuggerSession: debug, intent, onIntentConsumed }: { debuggerSession: DebuggerSession; intent: DebuggerLaunchIntent | null; onIntentConsumed: () => void }) {
  const session = debug.session
  const snapshot = session?.snapshot
  const [showLaunch, setShowLaunch] = useState(Boolean(intent))
  const [codeView, setCodeView] = useState<'source' | 'disassembly'>('source')
  const [location, setLocation] = useState('')
  const [breakKind, setBreakKind] = useState<'add' | 'opcode' | 'watch'>('add')
  const [condition, setCondition] = useState('')
  const [expression, setExpression] = useState('')
  const [keep, setKeep] = useState(false)
  const [command, setCommand] = useState('')
  const [mutationTarget, setMutationTarget] = useState<MutationTarget>('storage')
  const [mutationKey, setMutationKey] = useState('0')
  const [mutationValue, setMutationValue] = useState('')
  const [mutationConsent, setMutationConsent] = useState(false)
  const [result, setResult] = useState<DebugValue | undefined>()
  const [localError, setLocalError] = useState('')
  const [sourceStart, setSourceStart] = useState('1')
  const [aroundPc, setAroundPc] = useState('0')
  const codeViewport = useRef<HTMLDivElement>(null)
  const currentCodeRow = useRef<HTMLDivElement>(null)
  const disabled = !session || session.busy || debug.pending || session.state === 'cancelled' || session.state === 'failed'
  const stopped = !disabled && snapshot?.status.stopped === true

  async function send(method: DebuggerMethod, params: DebugRecord) {
    setLocalError('')
    const envelope = await debug.send(method, params)
    if (envelope?.result !== undefined) setResult(envelope.result)
    return envelope
  }
  const navigate = (action: NavigateAction) => void send('navigate', { action })
  const capability = snapshot?.capabilities
  const source = record(snapshot?.panes.source)
  const disassembly = record(snapshot?.panes.disassembly)
  const sourceRows = rows(source, 'items')
  const disassemblyRows = rows(disassembly, 'rows')
  const status = snapshot?.status || {}
  const fork = record(snapshot?.fork)
  const baseline = record(fork.baseline || status.baseline)
  const anchor = record(fork.anchor)
  const experiment = record(fork.experiment_result)
  const breakpoints = breakpointRows(snapshot?.breakpoints)
  const commandResult = resultOnly(session?.result ?? result)
  const sourceEnabled = capability?.source && sourceRows.length > 0
  const isSource = codeView === 'source' && sourceEnabled

  useLayoutEffect(() => {
    const viewport = codeViewport.current
    const row = currentCodeRow.current
    if (!viewport || !row) return
    // scrollIntoView can move every ancestor. Center only this code viewport,
    // preserving the page, sidebar, inspector, and horizontal scroll positions.
    const viewportRect = viewport.getBoundingClientRect()
    const rowRect = row.getBoundingClientRect()
    viewport.scrollTop += rowRect.top - viewportRect.top - viewport.clientTop - (viewport.clientHeight - rowRect.height) / 2
  }, [session?.id, session?.generation, status.step, status.pc, isSource, showLaunch])

  function breakpoint(location: string) { void send('breakpoint', { action: 'add', location }) }
  async function exportCapture() {
    const envelope = await send('export_capture', {})
    // A 202 acknowledgement may not contain the eventual export yet. The UI below
    // also offers download from the asynchronously streamed envelope.result.
    if (isCapture(envelope?.result) && envelope?.result && !envelope.busy) download(envelope.result)
  }
  function mutate() {
    if (!mutationConsent || !mutationValue) return
    const params: DebugRecord = { target: mutationTarget, value: mutationValue }
    if (mutationTarget === 'storage') params.key = mutationKey
    if (mutationTarget === 'memory' || mutationTarget === 'stack') {
      try {
        const integer = BigInt(mutationKey)
        if (integer < 0n || integer > 16777216n) throw new Error('Index / offset must be between 0 and 16777216.')
        if (mutationTarget === 'memory') params.offset = Number(integer)
        else params.index = Number(integer)
      } catch (cause) { setLocalError(cause instanceof Error ? cause.message : 'Invalid index / offset.'); return }
    }
    if (mutationTarget === 'variable') params.name = mutationKey
    void send('mutate', params)
  }
  function readCodeWindow() {
    try {
      const number = BigInt(isSource ? sourceStart : aroundPc)
      if (number < (isSource ? 1n : 0n) || number > 4294967295n) throw new Error('Enter a valid source line or uint32 PC.')
      void send('snapshot', isSource ? { sourceStart: number.toString() } : { aroundPc: number.toString() })
    } catch (cause) { setLocalError(cause instanceof Error ? cause.message : String(cause)) }
  }
  return <div className="flex h-full min-h-0 flex-col bg-slate-950 text-slate-300">
    <header className="flex shrink-0 flex-wrap items-center justify-between gap-2 border-b border-slate-800 bg-slate-900/60 px-4 py-3"><div className="flex flex-wrap items-center gap-3"><Bug size={18} className="text-indigo-400" /><h1 className="text-sm font-semibold text-slate-100">Debugger</h1><span className="rounded border border-emerald-900 bg-emerald-950/40 px-2 py-0.5 text-[10px] uppercase tracking-wider text-emerald-300">Local only · no broadcast</span>{session && <span className="text-xs text-slate-400">Worker: <strong className={session.busy ? 'text-amber-300' : 'text-slate-200'}>{session.state}{session.busy ? ' · busy' : ''}</strong></span>}</div><div className="flex gap-2"><button className={buttonClass} onClick={() => void debug.refreshCapabilities()}>Capabilities</button>{session && <button className={buttonClass} onClick={() => setShowLaunch(value => !value)}>{showLaunch ? 'Back to session' : 'New session'}</button>}</div></header>
    {(debug.error || session?.error || localError) && <div role="alert" className="shrink-0 border-b border-red-900/50 bg-red-950/30 px-4 py-2 text-xs text-red-300">{localError || debug.error || `${session?.error?.code}: ${session?.error?.message}`}</div>}
    {!session || showLaunch ? <div className="min-h-0 flex-1 overflow-auto"><DebuggerLaunch key={intent?.key || 'default'} debuggerSession={debug} intent={intent} onStarted={() => { setShowLaunch(false); onIntentConsumed() }} />{session && <p className="px-8 pb-4 text-xs text-amber-300">Starting another session disposes this worker first. Navigation between activities does not dispose it.</p>}{session && <button className="mx-8 mb-4 text-xs text-slate-500 underline" onClick={() => { setShowLaunch(false); onIntentConsumed() }}>Return to existing session</button>}</div> : <>
      <div className="flex shrink-0 flex-wrap items-center gap-1.5 border-b border-slate-800 px-3 py-2">
        <Tool label="Continue" icon={<Play size={13} />} disabled={!stopped} onClick={() => navigate('continue')} />
        <Tool label="Step opcode" icon={<StepForward size={13} />} disabled={!stopped} onClick={() => navigate('step_opcode')} />
        <Tool label="Step line" icon={<ArrowDownToLine size={13} />} disabled={!stopped || !capability?.source} reason={!capability?.source ? capability?.reason || 'Source mapping is unavailable.' : undefined} onClick={() => navigate('step_line')} />
        <Tool label="Next line" icon={<SkipForward size={13} />} disabled={!stopped || !capability?.source} reason={!capability?.source ? capability?.reason || 'Source mapping is unavailable.' : undefined} onClick={() => navigate('next_line')} />
        <Tool label="Finish frame" disabled={!stopped} onClick={() => navigate('finish')} />
        <Tool label="Restart" icon={<RotateCcw size={13} />} disabled={disabled || !capability?.restart} reason={!capability?.restart ? 'Restart is not supported for this session.' : undefined} onClick={() => navigate('restart')} />
        <Tool label="Cancel" icon={<Square size={12} />} disabled={session.state === 'cancelled'} onClick={() => void debug.dispose()} />
        <span className="flex-1" /><Tool label="Export capture" icon={<Download size={13} />} disabled={disabled || !snapshot?.fork} onClick={() => void exportCapture()} /><Tool label="Dispose" icon={<X size={13} />} onClick={() => void debug.dispose(true)} />
      </div>
      <div className="shrink-0 space-y-1 border-b border-slate-800 px-4 py-2 text-[11px]">
        <div className="flex flex-wrap gap-x-5 gap-y-1"><span>Baseline: <strong className={baseline.validated === true && debug.launchContext?.mode !== 'offline' ? 'text-emerald-300' : 'text-amber-300'}>{baseline.validated === true ? debug.launchContext?.mode === 'offline' ? 'captured receipts matched · offline / may be synthetic' : 'validated against captured receipts' : Object.keys(baseline).length ? 'unvalidated / synthetic' : 'not supplied'}</strong></span><span>EVM outcome: <strong>{experiment.success === true ? 'success' : experiment.success === false ? 'reverted / failed' : 'not reported'}</strong>{experiment.status !== undefined && <> · receipt status {display(experiment.status)}</>}</span><span>Worker completion: {status.finished ? status.ok === true ? 'completed' : 'exception' : 'pending'}</span><span className={fork.diverged || status.diverged ? 'text-amber-300' : 'text-slate-500'}>{fork.diverged || status.diverged ? 'Fork diverged · local experiment' : 'No local fork divergence reported'}</span></div>
        <div className="flex flex-wrap gap-x-4 gap-y-1 font-mono text-slate-500"><span>PC {display(status.pc)} · {display(status.mnemonic)} · {display(record(status.location).file)}:{display(record(status.location).line)}</span><span>gen {session.generation} · rev {session.revision} · seq {session.sequence} · {debug.transport}</span></div>
        {Object.keys(baseline).length > 0 && <details><summary className="cursor-pointer text-slate-400">Baseline scope / receipt validation (not consensus proof)</summary><div className="max-h-40 overflow-auto py-2 font-mono"><KeyValues value={{ validated: baseline.validated ?? null, validation: baseline.validation ?? null, scope: baseline.scope ?? null, state_validation: baseline.state_validation ?? null, identity: baseline.identity ?? null, execution: baseline.execution ?? null }} /></div></details>}
        {Object.keys(anchor).length > 0 && <details><summary className="cursor-pointer text-indigo-300">Anchor / transaction identity {display(anchor.block_hash || anchor.blockHash || anchor.hash)}</summary><div className="max-h-40 overflow-auto py-2 font-mono"><KeyValues value={anchor} /></div></details>}
        {capability?.reason && <p className="text-amber-300">{capability.reason}</p>}
        {session.state === 'cancelled' && <p className="text-amber-300">Worker cancelled. The last snapshot is retained for inspection; start a new session to execute again.</p>}
      </div>
      {snapshot ? <div className="flex min-h-0 flex-1 flex-col overflow-auto">
        <div className="grid h-[360px] shrink-0 grid-cols-1 overflow-auto lg:grid-cols-[minmax(0,1fr)_320px]">
          <section className="flex min-h-0 min-w-0 flex-col overflow-hidden" aria-label="Execution code">
            <div className="flex shrink-0 flex-wrap items-center gap-2 border-b border-slate-800 bg-slate-900/30 px-3 py-2"><button className={`${buttonClass} ${isSource ? '!text-indigo-300' : ''}`} disabled={!sourceEnabled} title={!sourceEnabled ? 'No mapped source available; disassembly remains available.' : undefined} onClick={() => setCodeView('source')}>Source</button><button className={`${buttonClass} ${!isSource ? '!text-indigo-300' : ''}`} onClick={() => setCodeView('disassembly')}>Disassembly</button><span className="min-w-0 flex-1 truncate text-xs text-slate-500">{isSource ? display(source.file) : 'EVM bytecode · breakpoint gutter'}</span><label className="sr-only" htmlFor="code-window">{isSource ? 'Source start line' : 'Around PC'}</label><input id="code-window" className={`${inputClass} !w-24 !py-1`} value={isSource ? sourceStart : aroundPc} onChange={event => isSource ? setSourceStart(event.target.value) : setAroundPc(event.target.value)} /><button className={buttonClass} disabled={disabled} onClick={readCodeWindow}>Read</button></div>
            <div ref={codeViewport} role="region" aria-label="Code viewport" className="min-h-0 flex-1 overflow-auto py-2 font-mono text-xs">{(isSource ? sourceRows : disassemblyRows).map((row, index) => {
              const rowLocation = isSource ? `${display(source.file)}:${display(row.line)}` : `*${display(row.pc)}`
              const marked = breakpoints.some(bp => bp.description.includes(rowLocation))
              return <div key={index} ref={row.current ? currentCodeRow : undefined} aria-current={row.current ? 'location' : undefined} className={`flex min-w-max items-start leading-6 ${row.current ? 'border-l-2 border-indigo-400 bg-indigo-500/15 text-indigo-100' : 'border-l-2 border-transparent text-slate-400'}`}><button disabled={!stopped} onClick={() => breakpoint(rowLocation)} aria-label={`Add breakpoint at ${rowLocation}`} title={`Add breakpoint at ${rowLocation}`} className={`w-7 shrink-0 text-center hover:text-red-300 ${marked ? 'text-red-400' : 'text-slate-700'}`}>●</button><span className={`w-14 shrink-0 select-none pr-3 text-right ${row.current ? 'text-indigo-300' : 'text-slate-600'}`}>{display(isSource ? row.line : row.pc)}</span><pre className="pr-6">{display(row.text || row.mnemonic)}</pre></div>
            })}{!(isSource ? sourceRows : disassemblyRows).length && <div className="px-4"><Empty text="No code window available at this state." /></div>}</div>
          </section>
          <aside className="min-h-0 overflow-auto border-l border-slate-800 bg-slate-900/20 p-3 text-xs">
            <PanelTitle title="Frames" /><DataTable columns={['index', 'name', 'kind', 'file', 'line', 'pc']} data={rows(snapshot.panes.frames, 'frames')} />
            <PanelTitle title="Locals" />{capability?.locals ? <DataTable columns={['name', 'type', 'type_name', 'display', 'value', 'reason']} data={rows(snapshot.panes.locals, 'locals')} /> : <Empty text={capability?.reason || 'Locals unavailable (raw, optimized, or via-IR code).'} />}
            <PanelTitle title="Arguments" />{record(snapshot.panes.arguments).decoded === false ? <Empty text="No ABI decode available; inspect Calldata below." /> : <DataTable columns={['name', 'type', 'value']} data={rows(snapshot.panes.arguments, 'args')} />}
            <PanelTitle title="Breakpoints" /><div className="space-y-2">{breakpoints.map((bp, index) => <div key={index} className="flex items-start justify-between gap-2"><span className="break-all font-mono text-slate-400">{bp.description}</span><button className="text-slate-500 hover:text-red-300 disabled:opacity-30" aria-label={`Delete breakpoint ${bp.id}`} disabled={disabled || !bp.id} onClick={() => void send('breakpoint', { action: 'delete', id: Number(bp.id) })}><X size={13} /></button></div>)}{!breakpoints.length && <Empty text="No breakpoints." />}</div>
            <select aria-label="Breakpoint type" className={`${inputClass} mt-3`} value={breakKind} onChange={event => setBreakKind(event.target.value as typeof breakKind)}><option value="add">Location / function</option><option value="opcode">Opcode</option><option value="watch">Watch expression</option></select><input aria-label="Breakpoint location" className={`${inputClass} mt-2`} placeholder={breakKind === 'add' ? 'File.sol:42 / withdraw / *0x20' : breakKind === 'opcode' ? 'SSTORE' : 'storage[0]'} value={location} onChange={event => setLocation(event.target.value)} /><input aria-label="Breakpoint condition" className={`${inputClass} mt-2`} placeholder="Condition (optional)" value={condition} onChange={event => setCondition(event.target.value)} /><div className="mt-2 flex flex-wrap gap-2"><button className={buttonClass} disabled={!stopped || !location.trim()} onClick={() => void send('breakpoint', { action: breakKind, ...(breakKind === 'opcode' ? { opcode: location } : breakKind === 'watch' ? { expression: location } : { location }), ...(condition ? { condition } : {}) })}>Add</button><button className={buttonClass} disabled={disabled} onClick={() => void send('breakpoint', { action: 'list' })}>Refresh</button><button className={buttonClass} disabled={!stopped || !location || breakKind !== 'add'} onClick={() => void send('navigate', { action: 'run_to', location })}>Run to</button></div>
          </aside>
        </div>
        <DebuggerPanels snapshot={snapshot} disabled={disabled} refresh={params => void send('snapshot', params)} />
        <details className="shrink-0 border-t border-slate-800 bg-slate-900/40"><summary className="cursor-pointer px-4 py-2 text-xs text-slate-400">Evaluate & local mutations</summary><div className="max-h-72 overflow-auto px-4 pb-3">
          <div className="grid gap-3 xl:grid-cols-2"><div><div className="flex flex-wrap items-center gap-2"><label className="sr-only" htmlFor="solidity-expression">Solidity expression</label><input id="solidity-expression" className={`${inputClass} min-w-40 flex-1`} placeholder="Solidity expression (discard by default)" value={expression} onChange={event => setExpression(event.target.value)} /><label className="flex items-center gap-1 text-xs text-amber-300"><input type="checkbox" checked={keep} onChange={event => setKeep(event.target.checked)} /> Keep</label><button className={buttonClass} disabled={!stopped || !capability?.solidity || !expression} title={!capability?.solidity ? capability?.reason || 'Solidity evaluation requires mapped, compatible build info.' : keep ? 'Keep evaluation side effects in the local worker' : 'Discard evaluation side effects'} onClick={() => void send('evaluate', { expression, keep })}>Evaluate</button></div><p className="mt-1 text-[10px] text-slate-500">Discard rolls back expression side effects. Keep explicitly changes only this worker.</p><div className="mt-2 flex gap-2"><Terminal size={14} className="mt-2 shrink-0 text-slate-500" /><input aria-label="Yul or vm command" className={inputClass} placeholder="Yul / vm.* command (backend allowlist only)" value={command} onChange={event => setCommand(event.target.value)} /><button className={buttonClass} disabled={!stopped || !command.trim()} onClick={() => void send('command', { text: command })}>Run</button></div></div>
          <div><div className="flex flex-wrap gap-2"><select aria-label="Mutation target" className={`${inputClass} !w-28`} value={mutationTarget} onChange={event => { setMutationTarget(event.target.value as MutationTarget); setMutationConsent(false) }}>{(['storage', 'memory', 'stack', 'gas', 'pc', 'variable'] as const).map(target => <option key={target}>{target}</option>)}</select>{!['gas', 'pc'].includes(mutationTarget) && <input aria-label="Mutation key" className={`${inputClass} min-w-24 flex-1`} placeholder={mutationTarget === 'variable' ? 'Variable name' : mutationTarget === 'storage' ? 'Slot' : mutationTarget === 'memory' ? 'Byte offset' : 'Stack index'} value={mutationKey} onChange={event => setMutationKey(event.target.value)} />}<input aria-label="Mutation value" className={`${inputClass} min-w-28 flex-1`} placeholder={mutationTarget === 'memory' ? 'Hex bytes 0x…' : 'Exact value'} value={mutationValue} onChange={event => setMutationValue(event.target.value)} /><button className={buttonClass} disabled={!stopped || !capability?.mutate || !mutationConsent || !mutationValue || (mutationTarget === 'variable' && !capability?.solidity)} title={!capability?.mutate ? 'Mutation is unavailable for this worker.' : undefined} onClick={mutate}>Write locally</button></div><label className="mt-2 flex items-center gap-2 text-[11px] text-amber-300"><input type="checkbox" checked={mutationConsent} onChange={event => setMutationConsent(event.target.checked)} /> Explicit local mutation; may diverge from the validated baseline. PC writes require JUMPDEST.</label></div></div>
          {(session.result !== undefined || result !== undefined) && <details className="mt-3 text-xs" open><summary className="cursor-pointer text-indigo-300">Last command result</summary><div className="mt-2 max-h-40 overflow-auto font-mono"><StructuredValue value={commandResult} /></div>{isCapture(session.result ?? result) && <button className={`${buttonClass} mt-2`} onClick={() => download((session.result ?? result)!)}>Download capture JSON</button>}</details>}
        </div></details>
      </div> : <div className="flex min-h-0 flex-1 items-center justify-center text-sm text-slate-500">{session.busy ? 'Preparing isolated debugger worker… Cancel remains available.' : 'No snapshot available. Inspect the worker error or start a new session.'}</div>}
    </>}
  </div>
}
function Tool({ label, icon, disabled, reason, onClick }: { label: string; icon?: React.ReactNode; disabled?: boolean; reason?: string; onClick: () => void }) { return <button className={buttonClass} title={reason || label} disabled={disabled} onClick={onClick}>{icon}{label}</button> }
function PanelTitle({ title }: { title: string }) { return <h2 className="mb-2 mt-4 first:mt-0 text-[10px] font-semibold uppercase tracking-wider text-slate-500">{title}</h2> }
function breakpointRows(value: DebugValue | undefined): { id: string; description: string }[] {
  const items = Array.isArray(value) ? value : record(value).breakpoints
  if (!Array.isArray(items)) return []
  return items.map(item => typeof item === 'string' ? { id: item.trim().match(/^\d+/)?.[0] || '', description: item } : { id: display(record(item).id), description: display(record(item).description) })
}
function resultOnly(value: DebugValue | undefined): DebugValue | undefined {
  const object = record(value)
  if (isCapture(value)) return { format: object.format, version: object.version, note: 'Capture ready to download.' }
  if (object.evaluation !== undefined) return object.evaluation
  if (object.mutation !== undefined) return object.mutation
  if (object.command !== undefined) return object.command
  const rest = Object.fromEntries(Object.entries(object).filter(([key]) => key !== 'snapshot'))
  return Object.keys(object).length ? rest : value
}
function isCapture(value: DebugValue | undefined) { const object = record(value); return Boolean(object.format && object.witness && object.plan) }
function download(value: DebugValue) {
  const blob = new Blob([safeStringify(value, 2)], { type: 'application/json' })
  const url = URL.createObjectURL(blob)
  const anchor = document.createElement('a'); anchor.href = url; anchor.download = 'chasm-debugger-capture.json'; anchor.click()
  setTimeout(() => URL.revokeObjectURL(url), 1000)
}
