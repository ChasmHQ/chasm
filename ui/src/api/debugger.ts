import type { DebuggerCapabilities, DebuggerLaunchRequest, DebuggerMethod, DebugRecord, SessionEnvelope } from '../types/debugger'
import { record, safeStringify } from '../types/debugger'

const ROOT = '/api/debugger'

// Preserve EVM quantities before JSON.parse can round a 256-bit integer.
export function parseDebuggerJson(text: string): unknown {
  return JSON.parse(text.replace(/"(?:\\.|[^"\\])*"|-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?/g, token => {
    if (/^-?\d+$/.test(token)) {
      const integer = BigInt(token)
      if (integer > BigInt(Number.MAX_SAFE_INTEGER) || integer < BigInt(Number.MIN_SAFE_INTEGER)) return `"${token}"`
    }
    return token
  }))
}

export class DebuggerApiError extends Error {
  readonly status: number
  readonly code?: string
  constructor(message: string, status: number, code?: string) { super(message); this.status = status; this.code = code }
}

export function createDebuggerApi() {
  let csrfToken = '' // Memory only; never persisted or put in a URL.
  async function request<T>(path: string, method = 'GET', body?: unknown, signal?: AbortSignal): Promise<T> {
    if (method !== 'GET' && !csrfToken) throw new Error('Local debugger ownership is not established. Refresh capabilities first.')
    const response = await fetch(`${ROOT}${path}`, {
      method, credentials: 'same-origin', signal,
      headers: method === 'GET' ? undefined : { 'Content-Type': 'application/json', 'X-Chasm-CSRF': csrfToken },
      body: body === undefined ? undefined : safeStringify(body),
    })
    const text = await response.text()
    let data: unknown
    try { data = text ? parseDebuggerJson(text) : null } catch { throw new DebuggerApiError(`Local debugger returned an invalid response (HTTP ${response.status}).`, response.status) }
    if (!response.ok) {
      const object = record(data)
      const error = record(object.error)
      throw new DebuggerApiError(String(error.message || object.message || (response.status === 409 ? 'Worker is busy or the session revision changed. Refresh the snapshot; the operation was not retried.' : `Debugger request failed (HTTP ${response.status}).`)), response.status, typeof error.code === 'string' ? error.code : undefined)
    }
    return data as T
  }
  return {
    async capabilities(signal?: AbortSignal) {
      const capabilities = await request<DebuggerCapabilities>('/capabilities', 'GET', undefined, signal)
      csrfToken = capabilities.csrfToken
      return capabilities
    },
    workspaceBuildInfo: () => request<{ id: string; name: string; kind: string }>('/build-info', 'POST', {}),
    resource: (kind: 'capture' | 'build_info', name: string, content: object) => request<{ id: string; name: string; kind: string }>('/resources', 'POST', { kind, name, content }),
    launch: (input: DebuggerLaunchRequest) => request<SessionEnvelope>('/sessions', 'POST', { ...input, requestId: crypto.randomUUID() }),
    snapshot: (id: string, signal?: AbortSignal) => request<SessionEnvelope>(`/sessions/${encodeURIComponent(id)}`, 'GET', undefined, signal),
    command: (session: SessionEnvelope, method: DebuggerMethod, params: DebugRecord) => request<SessionEnvelope>(`/sessions/${encodeURIComponent(session.id)}/commands`, 'POST', {
      requestId: crypto.randomUUID(), generation: session.generation, expectedRevision: session.revision, method, params,
    }),
    dispose: (id: string) => request<unknown>(`/sessions/${encodeURIComponent(id)}`, 'DELETE'),
    eventsUrl: (id: string) => {
      const url = new URL(`${ROOT}/sessions/${encodeURIComponent(id)}/events`, window.location.href)
      url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:'
      return url.href
    },
  }
}
