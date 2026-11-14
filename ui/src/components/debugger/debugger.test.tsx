// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, renderHook, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createDebuggerApi, parseDebuggerJson } from '../../api/debugger'
import { useDebuggerSession } from '../../hooks/useDebuggerSession'
import type { DebuggerSession } from '../../hooks/useDebuggerSession'
import { modeReason, newerEnvelope, safeStringify } from '../../types/debugger'
import type { DebuggerCapabilities, SessionEnvelope } from '../../types/debugger'
import { DebuggerLaunch } from './DebuggerLaunch'
import { DebuggerWorkspace } from './DebuggerWorkspace'

const capabilities: DebuggerCapabilities = { available: true, protocol: 'chasm-web-debugger/1', providers: [{ id: 'mainnet', label: 'Mainnet' }], examples: [{ id: 'synthetic', label: 'Synthetic offline replay' }, { id: 'source-demo', label: 'Synthetic withdrawal lab', buildInfoId: 'source-demo-build' }], modes: ['offline', 'fork', 'replay'], csrfToken: 'memory-only-token' }
const input = { mode: 'offline' as const, captureId: 'synthetic' }
function envelope(sequence = 1, generation = 1, id = 'session-a'): SessionEnvelope {
  return { id, sequence, generation, revision: sequence, state: 'paused', busy: false, error: null, snapshot: { status: { stopped: true }, capabilities: { source: false, solidity: false, locals: false, mutate: true, restart: true }, panes: {}, fork: null } }
}
function response(value: unknown, status = 200) { return new Response(safeStringify(value), { status, headers: { 'Content-Type': 'application/json' } }) }
class Socket {
  static OPEN = 1
  static instances: Socket[] = []
  readyState = 1
  onopen?: () => void
  onmessage?: (event: { data: string }) => void
  onclose?: () => void
  onerror?: () => void
  constructor() { Socket.instances.push(this) }
  close() { this.readyState = 3 }
  emit(value: unknown) { this.onmessage?.({ data: safeStringify(value) }) }
}

beforeEach(() => { Socket.instances = []; vi.stubGlobal('WebSocket', Socket) })
afterEach(() => { cleanup(); vi.unstubAllGlobals(); vi.restoreAllMocks() })

describe('exact quantities and protocol state', () => {
  it('preserves uint256 and unsafe JSON integers without changing strings', () => {
    const maximum = ((1n << 256n) - 1n).toString()
    expect(parseDebuggerJson(`{"word":${maximum},"negative":-9007199254740993,"safe":42,"hex":"0x123","text":"number 9007199254740993"}`)).toEqual({ word: maximum, negative: '-9007199254740993', safe: 42, hex: '0x123', text: 'number 9007199254740993' })
    expect(safeStringify({ maximum: BigInt(maximum) })).toBe(`{"maximum":"${maximum}"}`)
  })
  it('rejects old generations, sequences, revisions and different session identities', () => {
    const current = envelope(5, 2)
    expect(newerEnvelope(current, envelope(100, 1))).toBe(false)
    expect(newerEnvelope(current, envelope(4, 2))).toBe(false)
    expect(newerEnvelope(current, { ...envelope(6, 2), revision: 4 })).toBe(false)
    expect(newerEnvelope(current, envelope(6, 2, 'other'))).toBe(false)
    expect(newerEnvelope(current, envelope(1, 3))).toBe(true)
    expect(newerEnvelope(current, envelope(6, 2))).toBe(true)
  })
  it('explains unsupported capabilities and unknown protocols', () => {
    expect(modeReason({ ...capabilities, available: false, reason: 'Python missing' }, 'offline')).toBe('Python missing')
    expect(modeReason(capabilities, 'foundry')).toContain('not supported')
    expect(modeReason({ ...capabilities, protocol: 'unknown' }, 'offline')).toContain('protocol')
    expect(modeReason(capabilities, 'offline')).toBeNull()
  })
})

describe('API ownership and no mutation retries', () => {
  it('sends same-origin credentials and in-memory CSRF only in a header', async () => {
    const fetcher = vi.fn().mockResolvedValueOnce(response(capabilities)).mockResolvedValueOnce(response(envelope(), 202))
    vi.stubGlobal('fetch', fetcher)
    const api = createDebuggerApi()
    await api.capabilities()
    await api.command(envelope(), 'mutate', { target: 'storage', key: '0', value: '9007199254740993' })
    expect(fetcher.mock.calls[1][0]).toBe('/api/debugger/sessions/session-a/commands')
    const options = fetcher.mock.calls[1][1] as RequestInit
    expect(options.credentials).toBe('same-origin')
    expect(options.headers).toEqual({ 'Content-Type': 'application/json', 'X-Chasm-CSRF': 'memory-only-token' })
    expect(String(options.body)).toContain('"value":"9007199254740993"')
    expect(String(options.body)).not.toContain('memory-only-token')
    expect(localStorage.length).toBe(0)
  })
  it('does not retry 409 mutations', async () => {
    const fetcher = vi.fn().mockResolvedValueOnce(response(capabilities)).mockResolvedValueOnce(response({ error: { code: 'busy', message: 'Worker busy' } }, 409))
    vi.stubGlobal('fetch', fetcher)
    const api = createDebuggerApi(); await api.capabilities()
    await expect(api.command(envelope(), 'mutate', { target: 'gas', value: '1' })).rejects.toThrow('Worker busy')
    expect(fetcher).toHaveBeenCalledTimes(2)
  })
})

describe('persistent hook lifecycle', () => {
  it('ignores stale events and keeps a session through activity rerenders', async () => {
    vi.stubGlobal('fetch', vi.fn(async (url: string) => url.endsWith('/capabilities') ? response(capabilities) : response(envelope(), 202)))
    const { result, rerender } = renderHook(() => useDebuggerSession())
    await waitFor(() => expect(result.current.capabilities).not.toBeNull())
    await act(async () => { await result.current.launch(input) })
    await waitFor(() => expect(Socket.instances.length).toBe(1))
    act(() => { Socket.instances[0].emit(envelope(8, 2)); Socket.instances[0].emit(envelope(100, 1)); Socket.instances[0].emit(envelope(7, 2)) })
    expect(result.current.session?.sequence).toBe(8)
    rerender()
    expect(result.current.session?.id).toBe('session-a')
    expect(Socket.instances.length).toBe(1)
  })
  it('cancels independently of a pending continue and discards its late response', async () => {
    let finish: (value: Response) => void = () => undefined
    const fetcher = vi.fn(async (url: string, options?: RequestInit) => {
      if (url.endsWith('/capabilities')) return response(capabilities)
      if (options?.method === 'DELETE') return response({ cancelled: true })
      if (url.endsWith('/commands')) return new Promise<Response>(resolve => { finish = resolve })
      return response(envelope(), 202)
    })
    vi.stubGlobal('fetch', fetcher)
    const { result } = renderHook(() => useDebuggerSession())
    await waitFor(() => expect(result.current.capabilities).not.toBeNull())
    await act(async () => { await result.current.launch(input) })
    let running: Promise<SessionEnvelope | null>
    act(() => { running = result.current.send('navigate', { action: 'continue' }) })
    await act(async () => { await result.current.dispose() })
    expect(result.current.session?.state).toBe('cancelled')
    await act(async () => { finish(response(envelope(20))); await running })
    expect(result.current.session?.state).toBe('cancelled')
    expect(fetcher.mock.calls.filter(([, options]) => options?.method === 'DELETE')).toHaveLength(1)
  })
  it('fences reverse DELETE completions while replacement launch and cancel overlap', async () => {
    const deletes: ((value: Response) => void)[] = []
    let starts = 0
    vi.stubGlobal('fetch', vi.fn(async (url: string, options?: RequestInit) => {
      if (url.endsWith('/capabilities')) return response(capabilities)
      if (options?.method === 'DELETE') return new Promise<Response>(resolve => deletes.push(resolve))
      return response(envelope(1, 1, ++starts === 1 ? 'old-worker' : 'new-worker'), 202)
    }))
    const { result } = renderHook(() => useDebuggerSession())
    await waitFor(() => expect(result.current.capabilities).not.toBeNull())
    await act(async () => { await result.current.launch(input) })
    let replacement: Promise<boolean>
    let cancel: Promise<void>
    act(() => { replacement = result.current.launch(input) })
    act(() => { cancel = result.current.dispose() })
    expect(deletes).toHaveLength(2)
    // Cancel's DELETE completes first. The older launch must never allocate a VM.
    await act(async () => { deletes[1](response({})); await cancel })
    await act(async () => { deletes[0](response({})); await replacement })
    expect(starts).toBe(1)
    expect(result.current.session?.state).toBe('cancelled')
    await act(async () => { await result.current.launch(input) })
    expect(result.current.session?.id).toBe('new-worker')
    expect(result.current.pending).toBe(false)
  })
  it('does not let an older command finally unlock a newer command', async () => {
    const commands: ((value: Response) => void)[] = []
    vi.stubGlobal('fetch', vi.fn(async (url: string, options?: RequestInit) => {
      if (url.endsWith('/capabilities')) return response(capabilities)
      if (options?.method === 'DELETE') return response({})
      if (url.endsWith('/commands')) return new Promise<Response>(resolve => commands.push(resolve))
      return response(envelope(), 202)
    }))
    const { result } = renderHook(() => useDebuggerSession())
    await waitFor(() => expect(result.current.capabilities).not.toBeNull())
    await act(async () => { await result.current.launch(input) })
    let old: Promise<SessionEnvelope | null>
    act(() => { old = result.current.send('navigate', { action: 'continue' }) })
    await act(async () => { await result.current.dispose(); await result.current.launch(input) })
    let newer: Promise<SessionEnvelope | null>
    act(() => { newer = result.current.send('navigate', { action: 'step_opcode' }) })
    await act(async () => { commands[0](response(envelope(10))); await old })
    expect(result.current.pending).toBe(true)
    await act(async () => { commands[1](response(envelope(2))); await newer })
    expect(result.current.pending).toBe(false)
  })
  it('disposes an existing worker before launching another', async () => {
    const calls: string[] = []
    vi.stubGlobal('fetch', vi.fn(async (url: string, options?: RequestInit) => {
      calls.push(`${options?.method || 'GET'} ${url}`)
      if (url.endsWith('/capabilities')) return response(capabilities)
      if (options?.method === 'DELETE') return response({ cancelled: true })
      return response(envelope(), 202)
    }))
    const { result } = renderHook(() => useDebuggerSession())
    await waitFor(() => expect(result.current.capabilities).not.toBeNull())
    await act(async () => { await result.current.launch(input); await result.current.launch(input) })
    expect(calls).toEqual(['GET /api/debugger/capabilities', 'POST /api/debugger/sessions', 'DELETE /api/debugger/sessions/session-a', 'POST /api/debugger/sessions'])
  })
})

function launchDebug(overrides: Partial<DebuggerSession> = {}): DebuggerSession {
  return { capabilities, session: null, launchContext: null, pending: false, error: null, transport: 'connecting', launch: vi.fn().mockResolvedValue(true), send: vi.fn(), dispose: vi.fn(), refreshCapabilities: vi.fn(), upload: vi.fn(), workspaceBuildInfo: vi.fn(), ...overrides }
}

describe('execution code follow', () => {
  it('centers the current row only in the code viewport when execution advances', () => {
    const rectangle = (top: number, height: number) => ({ top, height, left: 0, bottom: top + height, right: 600, width: 600, x: 0, y: top, toJSON: () => ({}) })
    vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function (this: HTMLElement) {
      if (this.getAttribute('aria-label') === 'Code viewport') return rectangle(200, 240)
      return rectangle(this.getAttribute('aria-current') === 'location' ? 600 : 0, 24)
    })
    vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockImplementation(function (this: HTMLElement) { return this.getAttribute('aria-label') === 'Code viewport' ? 240 : 0 })
    const debug = launchDebug({ session: envelope() })
    const session = debug.session!
    session.snapshot!.capabilities.source = true
    session.snapshot!.status = { stopped: true, step: 1, pc: 312 }
    session.snapshot!.panes.source = { file: 'Vault.sol', items: [{ line: 16, text: 'before', current: false }, { line: 17, text: 'current', current: true }] }
    const props = { debuggerSession: debug, intent: null, onIntentConsumed: vi.fn() }
    const { rerender } = render(<DebuggerWorkspace {...props} />)
    const viewport = screen.getByRole('region', { name: 'Code viewport' })
    expect(viewport.scrollTop).toBe(292)
    expect(viewport.querySelector('[aria-current="location"]')?.textContent).toContain('17')
    const workspace = viewport.closest('section')!.parentElement!.parentElement!
    workspace.scrollTop = 19
    viewport.scrollTop = 47
    viewport.scrollLeft = 23
    // Cached envelopes and unrelated state updates must respect manual scrolling.
    rerender(<DebuggerWorkspace {...props} debuggerSession={{ ...debug, session: { ...session, sequence: 2 } }} />)
    expect(viewport.scrollTop).toBe(47)
    const advanced = { ...session, snapshot: { ...session.snapshot!, status: { stopped: true, step: 2, pc: 313 } } }
    rerender(<DebuggerWorkspace {...props} debuggerSession={{ ...debug, session: advanced }} />)
    expect(viewport.scrollTop).toBe(339)
    expect(viewport.scrollLeft).toBe(23)
    expect(workspace.scrollTop).toBe(19)
  })
})

describe('App activity integration', () => {
  it('opens Debugger and retains a real session across activity changes', async () => {
    vi.stubGlobal('fetch', vi.fn(async (url: string) => {
      if (url.endsWith('/capabilities')) return response(capabilities)
      if (url.endsWith('/sessions')) return response(envelope(), 202)
      if (url.includes('/api/debugger/sessions/')) return response(envelope())
      if (url.endsWith('/fork/status')) return response({ running: false })
      if (url.includes('/oz/')) return response({ local: false, globalVersions: [] })
      return response({ jsonrpc: '2.0', id: 1, result: '0x7a69' })
    }))
    const { default: App } = await import('../../App')
    render(<App />)
    fireEvent.click(screen.getByRole('button', { name: 'Debugger activity' }))
    await waitFor(() => expect((screen.getByRole('button', { name: 'Start local session' }) as HTMLButtonElement).disabled).toBe(false))
    fireEvent.click(screen.getByRole('button', { name: 'Start local session' }))
    await waitFor(() => expect(screen.getByRole('button', { name: 'Step opcode' })).toBeDefined())
    fireEvent.click(screen.getByTitle('Converter & Utils'))
    fireEvent.click(screen.getByRole('button', { name: 'Debugger activity' }))
    expect(screen.getByRole('button', { name: 'Step opcode' })).toBeDefined()
    expect(screen.queryByRole('button', { name: 'Start local session' })).toBeNull()
  })
})

describe('launch capability gates', () => {
  it('launches a real offline example with its paired build-info id', async () => {
    const debug = launchDebug()
    render(<DebuggerLaunch debuggerSession={debug} intent={null} onStarted={vi.fn()} />)
    fireEvent.change(screen.getByLabelText('Capture'), { target: { value: 'source-demo' } })
    fireEvent.click(screen.getByRole('button', { name: 'Start local session' }))
    await waitFor(() => expect(debug.launch).toHaveBeenCalledWith({ mode: 'offline', captureId: 'source-demo', buildInfoId: 'source-demo-build' }))
  })
  it('sends only replay fields, without fork transaction defaults', async () => {
    const debug = launchDebug()
    render(<DebuggerLaunch debuggerSession={debug} intent={{ key: 'tx', mode: 'replay', txHash: `0x${'a'.repeat(64)}`, providerId: 'mainnet', chainId: 1, local: false, originLabel: 'Saved tx' }} onStarted={vi.fn()} />)
    fireEvent.click(screen.getByRole('checkbox'))
    fireEvent.click(screen.getByRole('button', { name: 'Start local session' }))
    await waitFor(() => expect(debug.launch).toHaveBeenCalledWith({ mode: 'replay', txHash: `0x${'a'.repeat(64)}`, providerId: 'mainnet' }))
  })
  it('serializes fork anchor as safe integer and EVM quantities as strings', async () => {
    const debug = launchDebug()
    render(<DebuggerLaunch debuggerSession={debug} intent={{ key: 'fork', mode: 'fork', providerId: 'mainnet', local: false, originLabel: 'Saved call', call: { caller: `0x${'1'.repeat(40)}`, to: `0x${'2'.repeat(40)}`, data: '0x1234', value: '9007199254740993', gas: '100000' } }} onStarted={vi.fn()} />)
    fireEvent.change(screen.getByLabelText('Anchor block (explicit number)'), { target: { value: '21000000' } })
    fireEvent.click(screen.getByRole('checkbox'))
    fireEvent.click(screen.getByRole('button', { name: 'Start local session' }))
    await waitFor(() => expect(debug.launch).toHaveBeenCalledWith({ mode: 'fork', providerId: 'mainnet', blockNumber: 21000000, to: `0x${'2'.repeat(40)}`, caller: `0x${'1'.repeat(40)}`, data: '0x1234', value: '9007199254740993', gas: 100000, nonce: 0, gasPrice: '0' }))
  })
  it('sends a Foundry target only after explicit trust confirmation', async () => {
    const debug = launchDebug({ capabilities: { ...capabilities, modes: ['foundry'] } })
    render(<DebuggerLaunch debuggerSession={debug} intent={null} onStarted={vi.fn()} />)
    fireEvent.click(screen.getByRole('button', { name: 'Local Foundry' }))
    fireEvent.change(screen.getByLabelText('Trusted workspace target'), { target: { value: 'test/Counter.t.sol' } })
    fireEvent.click(screen.getByRole('checkbox'))
    fireEvent.click(screen.getByRole('button', { name: 'Start local session' }))
    await waitFor(() => expect(debug.launch).toHaveBeenCalledWith({ mode: 'foundry', target: 'test/Counter.t.sol' }))
  })
  it('never silently routes a local31337 saved request into a mainnet fork', () => {
    render(<DebuggerLaunch debuggerSession={launchDebug()} intent={{ key: 'local', mode: 'fork', chainId: 31337, local: true, originLabel: 'Saved request', call: { data: '0x', value: '0' } }} onStarted={vi.fn()} />)
    expect((screen.getByRole('button', { name: 'Start local session' }) as HTMLButtonElement).disabled).toBe(true)
    expect(screen.getByRole('status').textContent).toContain('trusted local Foundry')
  })
  it('disables unavailable worker actions with the capability reason', () => {
    render(<DebuggerLaunch debuggerSession={launchDebug({ capabilities: { ...capabilities, available: false, reason: 'Worker not installed' } })} intent={null} onStarted={vi.fn()} />)
    expect((screen.getByRole('button', { name: 'Start local session' }) as HTMLButtonElement).disabled).toBe(true)
    expect(screen.getByRole('status').textContent).toBe('Worker not installed')
  })
})
