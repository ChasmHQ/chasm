export type DebugValue = null | boolean | number | string | DebugValue[] | { [key: string]: DebugValue }
export type DebugRecord = { [key: string]: DebugValue }
export type DebugMode = 'offline' | 'replay' | 'fork' | 'foundry'
export type WorkerState = 'preparing' | 'running' | 'paused' | 'finished' | 'failed' | 'cancelled'

export interface DebuggerCapabilities {
  available: boolean
  protocol: string
  providers: { id: string; label: string }[]
  examples: { id: string; label: string; buildInfoId?: string }[]
  modes: DebugMode[] | Record<string, boolean | { available?: boolean; reason?: string }>
  reason?: string
  csrfToken: string
}

export interface DebuggerSnapshot {
  status: DebugRecord
  capabilities: { source: boolean; solidity: boolean; locals: boolean; mutate: boolean; restart: boolean; reason?: string }
  panes: Partial<Record<'stack' | 'memory' | 'storage' | 'frames' | 'locals' | 'arguments' | 'disassembly' | 'source' | 'calldata' | 'gas' | 'logs' | 'diff', DebugValue>>
  fork: DebugValue
  breakpoints?: DebugValue
}

export interface SessionEnvelope {
  id: string
  generation: number
  revision: number
  sequence: number
  state: WorkerState
  busy: boolean
  snapshot: DebuggerSnapshot | null
  error: { code: string; message: string } | null
  result?: DebugValue
}

export interface DebuggerLaunchRequest {
  mode: DebugMode
  captureId?: string
  buildInfoId?: string
  txHash?: string
  providerId?: string
  blockNumber?: number
  to?: string
  caller?: string
  data?: string
  value?: string
  gas?: number
  nonce?: number
  gasPrice?: string
  target?: string
  match?: string
}

// A saved launch intent contains immutable values and aliases, never an RPC URL or key.
export interface DebuggerLaunchIntent {
  key: string
  mode: 'replay' | 'fork'
  txHash?: string
  providerId?: string
  chainId?: number
  originLabel: string
  local: boolean
  call?: { to?: string; caller?: string; data: string; value: string; gas?: string }
}

export type DebuggerMethod = 'snapshot' | 'navigate' | 'breakpoint' | 'evaluate' | 'mutate' | 'command' | 'export_capture'
export type NavigateAction = 'continue' | 'step_opcode' | 'step_line' | 'next_line' | 'finish' | 'restart' | 'run_to'
export type MutationTarget = 'storage' | 'memory' | 'stack' | 'gas' | 'pc' | 'variable'

export function record(value: unknown): DebugRecord {
  return value !== null && typeof value === 'object' && !Array.isArray(value) ? value as DebugRecord : {}
}

export function rows(value: unknown, ...keys: string[]): DebugRecord[] {
  if (Array.isArray(value)) return value.map(record)
  const object = record(value)
  for (const key of keys) if (Array.isArray(object[key])) return (object[key] as DebugValue[]).map(record)
  return []
}

export function display(value: unknown): string {
  if (value === undefined || value === null) return '—'
  if (typeof value === 'object') return safeStringify(value)
  return String(value)
}

export function safeStringify(value: unknown, space?: number): string {
  return JSON.stringify(value, (_key, item: unknown) => typeof item === 'bigint' ? item.toString() : item, space) ?? ''
}

export function modeReason(capabilities: DebuggerCapabilities | null, mode: DebugMode): string | null {
  if (!capabilities) return 'Checking local debugger capabilities…'
  if (!capabilities.available) return capabilities.reason || 'Debugger worker is unavailable.'
  if (capabilities.protocol !== 'chasm-web-debugger/1') return 'Unsupported debugger protocol.'
  if (Array.isArray(capabilities.modes)) return capabilities.modes.includes(mode) ? null : `${mode} mode is not supported by this worker.`
  const capability = capabilities.modes[mode]
  if (capability === true || (typeof capability === 'object' && capability?.available)) return null
  return typeof capability === 'object' && capability?.reason ? capability.reason : `${mode} mode is not supported by this worker.`
}

export function newerEnvelope(current: SessionEnvelope | null, incoming: SessionEnvelope): boolean {
  if (!current) return true
  if (current.id !== incoming.id) return false
  if (incoming.generation !== current.generation) return incoming.generation > current.generation
  return incoming.sequence > current.sequence && incoming.revision >= current.revision
}
