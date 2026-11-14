import { useState } from 'react'
import { Bug, Upload, ShieldCheck, Loader2 } from 'lucide-react'
import type { DebuggerSession } from '../../hooks/useDebuggerSession'
import { modeReason } from '../../types/debugger'
import type { DebuggerLaunchIntent, DebuggerLaunchRequest, DebugMode } from '../../types/debugger'
import { parseDebuggerJson } from '../../api/debugger'

export const inputClass = 'w-full rounded border border-slate-700 bg-slate-950 px-3 py-2 text-xs text-slate-200 outline-none focus:border-indigo-500 disabled:opacity-50'
export const buttonClass = 'inline-flex items-center justify-center gap-1.5 rounded border border-slate-700 bg-slate-900 px-3 py-1.5 text-xs text-slate-200 hover:bg-slate-800 disabled:cursor-not-allowed disabled:opacity-40'

export function DebuggerLaunch({ debuggerSession: debug, intent, onStarted }: { debuggerSession: DebuggerSession; intent: DebuggerLaunchIntent | null; onStarted: () => void }) {
  const [mode, setMode] = useState<DebugMode>(intent?.mode || 'offline')
  const [captureId, setCaptureId] = useState('synthetic')
  const [buildInfoId, setBuildInfoId] = useState('')
  const [uploads, setUploads] = useState<{ id: string; name: string; kind: string }[]>([])
  const [providerId, setProviderId] = useState(intent?.providerId || '')
  const [txHash, setTxHash] = useState(intent?.txHash || '')
  const [blockNumber, setBlockNumber] = useState('')
  const [to, setTo] = useState(intent?.call?.to || '')
  const [caller, setCaller] = useState(intent?.call?.caller || '')
  const [data, setData] = useState(intent?.call?.data || '0x')
  const [value, setValue] = useState(intent?.call?.value || '0')
  const [gas, setGas] = useState(() => {
    const saved = intent?.call?.gas
    if (!saved) return '1000000'
    try { return BigInt(saved).toString() } catch { return saved }
  })
  const [nonce, setNonce] = useState('0')
  const [gasPrice, setGasPrice] = useState('0')
  const [target, setTarget] = useState('')
  const [match, setMatch] = useState('')
  const [error, setError] = useState('')
  const [uploading, setUploading] = useState(false)
  const [building, setBuilding] = useState(false)
  const [consent, setConsent] = useState(false)
  const [networkConsent, setNetworkConsent] = useState(false)
  const capabilityReason = modeReason(debug.capabilities, mode)
  const localOrigin = intent?.local && (mode === 'fork' || mode === 'replay')
  const providerRequired = mode === 'fork' || mode === 'replay'
  const disabledReason = capabilityReason || (localOrigin ? 'This request originated on local chain 31337. Reproduce it in a trusted local Foundry test; mainnet replay/fork is not a substitute.' : null) ||
    (providerRequired && !providerId ? 'Choose a server-configured provider alias. The originating RPC URL is never forwarded.' : null) ||
    (providerRequired && !networkConsent ? 'Explicitly allow historical reads through the selected server alias (no transaction is sent).' : null) ||
    (mode === 'foundry' && !consent ? 'Confirm that this workspace target is trusted before executing local Foundry code.' : null)

  async function upload(file: File | undefined, kind: 'capture' | 'build_info') {
    if (!file) return
    setUploading(true); setError('')
    try {
      if (file.size > 8 * 1024 * 1024) throw new Error('Upload exceeds the 8 MiB resource limit.')
      const content = parseDebuggerJson(await file.text())
      if (!content || typeof content !== 'object' || Array.isArray(content)) throw new Error('Expected a JSON object.')
      const resource = await debug.upload(kind, file.name, content)
      setUploads(previous => [...previous, resource])
      if (kind === 'capture') setCaptureId(resource.id)
      else setBuildInfoId(resource.id)
    } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
    finally { setUploading(false) }
  }

  async function workspaceBuild() {
    setBuilding(true); setError('')
    try {
      const resource = await debug.workspaceBuildInfo()
      setUploads(previous => [...previous, resource])
      setBuildInfoId(resource.id)
    } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
    finally { setBuilding(false) }
  }

  async function launch() {
    setError('')
    try {
      if (disabledReason) throw new Error(disabledReason)
      const request: DebuggerLaunchRequest = { mode }
      const example = debug.capabilities?.examples.find(item => item.id === captureId)
      if (mode === 'offline') request.captureId = captureId
      const pairedBuildInfo = mode === 'offline' ? example?.buildInfoId : undefined
      if (mode !== 'foundry' && (buildInfoId || pairedBuildInfo)) request.buildInfoId = buildInfoId || pairedBuildInfo
      if (providerRequired) request.providerId = providerId
      if (mode === 'replay') {
        if (!/^0x[\da-fA-F]{64}$/.test(txHash)) throw new Error('Enter a 32-byte transaction hash.')
        request.txHash = txHash
      }
      if (mode === 'fork') {
        request.blockNumber = safeInteger(blockNumber, 'Anchor block')
        request.value = unsigned(value, 'Value')
        request.gas = safeInteger(gas, 'Gas', 1)
        request.nonce = safeInteger(nonce, 'Nonce')
        request.gasPrice = unsigned(gasPrice, 'Gas price')
        if (!/^0x[\da-fA-F]{40}$/.test(to) || !/^0x[\da-fA-F]{40}$/.test(caller)) throw new Error('From and To must be 20-byte addresses.')
        if (!/^0x([\da-fA-F]{2})*$/.test(data)) throw new Error('Calldata must be even-length hex bytes.')
        Object.assign(request, { to, caller, data })
      }
      if (mode === 'foundry') {
        if (!target.endsWith('.sol') || /(^[\\/]|:|(^|[\\/])\.\.([\\/]|$))/.test(target)) throw new Error('Use a workspace-relative .sol target, without parent traversal.')
        request.target = target
        if (match) request.match = match
      }
      if (await debug.launch(request)) onStarted()
    } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
  }

  return <section className="mx-auto w-full max-w-5xl p-4 md:p-8">
    <div className="mb-6 flex items-start gap-3"><div className="rounded-lg bg-indigo-500/10 p-3 text-indigo-400"><Bug size={26} /></div><div><h1 className="text-xl font-semibold text-slate-100">Debug locally</h1><p className="mt-1 text-sm text-slate-400">Replay, inspect, and experiment in an isolated debugger worker. Never signs or broadcasts transactions.</p></div></div>
    {intent && <div className="mb-4 rounded border border-indigo-900/70 bg-indigo-950/20 p-3 text-xs text-indigo-200"><p>Saved launch context · {intent.originLabel} {intent.chainId ? `· chain ${intent.chainId}` : ''}</p>{intent.txHash && <p className="mt-1 break-all font-mono">{intent.txHash}</p>}<p className="mt-1 text-slate-400">Immutable request values were copied when Debug locally was selected. No RPC URL or wallet key is shared with the debugger.</p></div>}
    <div className="rounded-lg border border-slate-800 bg-slate-900/40 p-4 md:p-6">
      <div className="mb-5 flex flex-wrap gap-2">{(['offline', 'replay', 'fork', 'foundry'] as const).map(item => <button key={item} onClick={() => setMode(item)} aria-pressed={mode === item} className={`${buttonClass} ${mode === item ? '!border-indigo-500 !bg-indigo-500/15 !text-indigo-300' : ''}`}>{({ offline: 'Offline replay', replay: 'Historical transaction', fork: 'Fork call', foundry: 'Local Foundry' })[item]}</button>)}</div>
      <div className="grid gap-4 md:grid-cols-2">
        {mode === 'offline' && <Field label="Capture"><select className={inputClass} value={captureId} onChange={event => { setCaptureId(event.target.value); setBuildInfoId('') }}><option value="synthetic">Synthetic offline replay</option>{debug.capabilities?.examples.filter(example => example.id !== 'synthetic').map(example => <option key={example.id} value={example.id}>{example.label}</option>)}{uploads.filter(item => item.kind === 'capture').map(item => <option key={item.id} value={item.id}>{item.name}</option>)}</select></Field>}
        {mode !== 'foundry' && <Field label="Build info (optional source mapping)"><select className={inputClass} value={buildInfoId} onChange={event => setBuildInfoId(event.target.value)}><option value="">{mode === 'offline' && debug.capabilities?.examples.find(item => item.id === captureId)?.buildInfoId ? 'Bundled build info (auto-paired)' : 'No build info — raw EVM view'}</option>{uploads.filter(item => item.kind === 'build_info').map(item => <option key={item.id} value={item.id}>{item.name}</option>)}</select></Field>}
        {providerRequired && <Field label="Configured provider alias"><select className={inputClass} value={providerId} onChange={event => setProviderId(event.target.value)}><option value="">Choose provider — no URL inference</option>{debug.capabilities?.providers.map(provider => <option key={provider.id} value={provider.id}>{provider.label} ({provider.id})</option>)}</select></Field>}
        {mode === 'replay' && <Field label="Historical transaction hash"><input className={inputClass} value={txHash} onChange={event => setTxHash(event.target.value)} placeholder="0x…" /></Field>}
        {mode === 'fork' && <><Field label="Anchor block (explicit number)"><input className={inputClass} value={blockNumber} onChange={event => setBlockNumber(event.target.value)} placeholder="e.g. 21000000" /></Field><Field label="From / caller"><input className={inputClass} value={caller} onChange={event => setCaller(event.target.value)} placeholder="0x…" /></Field><Field label="To"><input className={inputClass} value={to} onChange={event => setTo(event.target.value)} placeholder="0x…" /></Field><Field label="Calldata"><textarea className={`${inputClass} font-mono`} value={data} onChange={event => setData(event.target.value)} /></Field></>}
        {mode === 'foundry' && <><Field label="Trusted workspace target"><input className={inputClass} value={target} onChange={event => setTarget(event.target.value)} placeholder="test/Counter.t.sol" /></Field><Field label="Test match (optional)"><input className={inputClass} value={match} onChange={event => setMatch(event.target.value)} placeholder="testCounter" /></Field></>}
        {mode === 'fork' && <><Field label="Value (wei, exact)"><input className={inputClass} value={value} onChange={event => setValue(event.target.value)} /></Field><Field label="Gas limit"><input className={inputClass} value={gas} onChange={event => setGas(event.target.value)} /></Field><Field label="Nonce (explicit)"><input className={inputClass} value={nonce} onChange={event => setNonce(event.target.value)} /></Field><Field label="Gas price (wei, explicit)"><input className={inputClass} value={gasPrice} onChange={event => setGasPrice(event.target.value)} /></Field></>}
      </div>
      <div className="mt-4 flex flex-wrap gap-3">{(['capture', 'build_info'] as const).map(kind => <label key={kind} className={`${buttonClass} cursor-pointer`}><Upload size={13} /> Upload {kind === 'capture' ? 'capture' : 'build info'}<input type="file" accept=".json,application/json" className="sr-only" aria-label={`Upload ${kind}`} disabled={uploading || !debug.capabilities?.available} onChange={event => { void upload(event.target.files?.[0], kind); event.target.value = '' }} /></label>)}{uploading && <Loader2 size={16} className="animate-spin text-indigo-400" />}<button className={buttonClass} disabled={building || !debug.capabilities?.available} title="Compile trusted local workspace into an owner-scoped build-info resource. Compilation is not cancelled by the debugger Cancel button." onClick={() => void workspaceBuild()}>{building && <Loader2 size={13} className="animate-spin" />}{building ? 'Building workspace info…' : 'Load workspace build info'}</button></div>
      {providerRequired && <label className="mt-4 flex items-start gap-2 text-xs text-slate-400"><input type="checkbox" checked={networkConsent} onChange={event => setNetworkConsent(event.target.checked)} /> Allow read-only historical RPC access through the selected configured alias. Fork/replay currently supports mainnet, not arbitrary local chains.</label>}
      {mode === 'foundry' && <label className="mt-4 flex items-start gap-2 text-xs text-amber-300"><input type="checkbox" checked={consent} onChange={event => setConsent(event.target.checked)} /> I trust this workspace and target. Foundry may execute local test/build code; no network transaction will be sent.</label>}
      {disabledReason && <p className="mt-4 text-xs text-amber-300" role="status">{disabledReason}</p>}
      {error && <p className="mt-4 break-words text-xs text-red-400" role="alert">{error}</p>}
      <div className="mt-6 flex flex-wrap items-center justify-between gap-3"><span className="flex items-center gap-2 text-xs text-slate-500"><ShieldCheck size={14} /> Local worker · cookie-owned · no signing</span><button className={`${buttonClass} !border-indigo-500 !bg-indigo-600 hover:!bg-indigo-500`} disabled={Boolean(disabledReason) || uploading || debug.pending} title={disabledReason || undefined} onClick={() => void launch()}>{debug.pending ? <Loader2 size={14} className="animate-spin" /> : <Bug size={14} />} Start local session</button></div>
    </div>
  </section>
}

function Field({ label, children }: { label: string; children: React.ReactNode }) { return <label className="flex flex-col gap-1.5 text-xs text-slate-400"><span>{label}</span>{children}</label> }
function unsigned(value: string, label: string): string {
  if (!/^(0x[\da-fA-F]+|\d+)$/.test(value)) throw new Error(`${label} must be a non-negative integer.`)
  const parsed = BigInt(value)
  if (parsed >= 1n << 256n) throw new Error(`${label} exceeds uint256.`)
  return parsed.toString()
}
function safeInteger(value: string, label: string, minimum = 0): number {
  const parsed = BigInt(unsigned(value, label))
  if (parsed < BigInt(minimum) || parsed > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error(`${label} must fit the API's safe integer range.`)
  return Number(parsed)
}
