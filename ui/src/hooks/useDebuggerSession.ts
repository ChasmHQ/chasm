import { useCallback, useEffect, useRef, useState } from 'react'
import { createDebuggerApi, parseDebuggerJson } from '../api/debugger'
import { newerEnvelope } from '../types/debugger'
import type { DebuggerCapabilities, DebuggerLaunchRequest, DebuggerMethod, DebugRecord, SessionEnvelope } from '../types/debugger'

export function useDebuggerSession() {
  const [api] = useState(createDebuggerApi)
  const [capabilities, setCapabilities] = useState<DebuggerCapabilities | null>(null)
  const [session, setSession] = useState<SessionEnvelope | null>(null)
  const [pending, setPending] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [transport, setTransport] = useState<'connecting' | 'websocket' | 'polling'>('connecting')
  const [connectionEpoch, setConnectionEpoch] = useState(0)
  const [launchContext, setLaunchContext] = useState<DebuggerLaunchRequest | null>(null)
  const suspended = useRef(false)
  const current = useRef<SessionEnvelope | null>(null)
  const operation = useRef<symbol | null>(null)
  const epoch = useRef(0)
  const disposed = useRef(false)
  const mounted = useRef(true)

  const accept = useCallback((incoming: SessionEnvelope) => {
    if (!mounted.current || !newerEnvelope(current.current, incoming)) return
    current.current = incoming
    setSession(incoming)
  }, [])

  const refreshCapabilities = useCallback(async () => {
    try {
      const next = await api.capabilities()
      if (mounted.current) { setCapabilities(next); setError(null) }
    } catch (cause) { if (mounted.current) setError(message(cause)) }
  }, [api])

  useEffect(() => {
    mounted.current = true
    void refreshCapabilities()
    return () => { mounted.current = false }
  }, [refreshCapabilities])

  const sessionId = session?.id
  const cancelled = session?.state === 'cancelled'
  useEffect(() => {
    if (!sessionId || cancelled) return
    let stopped = false
    let socket: WebSocket | undefined
    let timer: ReturnType<typeof setTimeout> | undefined
    const controller = new AbortController()
    const version = epoch.current
    const receive = (envelope: SessionEnvelope) => {
      if (!stopped && !suspended.current && epoch.current === version && envelope.id === sessionId) accept(envelope)
    }
    const poll = async () => {
      try { receive(await api.snapshot(sessionId, controller.signal)) }
      catch (cause) { if (!stopped) setError(`Snapshot polling: ${message(cause)}`) }
      if (!stopped) timer = setTimeout(poll, socket?.readyState === WebSocket.OPEN ? 4000 : 1000)
    }
    try {
      socket = new WebSocket(api.eventsUrl(sessionId))
      socket.onopen = () => { if (!stopped) setTransport('websocket') }
      socket.onmessage = event => {
        try { receive(parseDebuggerJson(String(event.data)) as SessionEnvelope) }
        catch { if (!stopped) setError('Invalid debugger event; polling will recover the latest snapshot.') }
      }
      socket.onclose = () => { if (!stopped) setTransport('polling') }
      socket.onerror = () => { if (!stopped) setTransport('polling') }
    } catch { setTransport('polling') }
    timer = setTimeout(poll, 500)
    return () => {
      stopped = true
      controller.abort()
      if (timer) clearTimeout(timer)
      socket?.close()
    }
  }, [api, accept, sessionId, cancelled, connectionEpoch])

  const launch = useCallback(async (input: DebuggerLaunchRequest) => {
    if (operation.current) return false
    const token = Symbol('launch')
    operation.current = token
    setPending(true)
    setError(null)
    const version = ++epoch.current
    const previous = current.current
    suspended.current = true
    try {
      // Invalidate old events at disposal intent, not after the DELETE resolves.
      if (previous && !disposed.current) await api.dispose(previous.id)
      if (version !== epoch.current || operation.current !== token) return false
      current.current = null
      disposed.current = false
      setSession(null)
      const next = await api.launch(input)
      if (version !== epoch.current) return false
      suspended.current = false
      setLaunchContext({ ...input })
      accept(next)
      return true
    } catch (cause) {
      if (mounted.current && version === epoch.current) { suspended.current = false; setConnectionEpoch(value => value + 1); setError(message(cause)) }
      return false
    }
    finally { if (operation.current === token) { operation.current = null; if (mounted.current) setPending(false) } }
  }, [api, accept])

  const send = useCallback(async (method: DebuggerMethod, params: DebugRecord = {}) => {
    const previous = current.current
    if (!previous || disposed.current) return null
    if (operation.current || previous.busy) {
      setError('Worker is busy. Cancel independently or wait for the current operation; commands are never retried.')
      return null
    }
    const token = Symbol('command')
    operation.current = token
    setPending(true)
    setError(null)
    const version = epoch.current
    try {
      const next = await api.command(previous, method, params)
      if (version !== epoch.current) return null
      accept(next)
      return next
    } catch (cause) { if (version === epoch.current) setError(message(cause)); return null }
    finally { if (operation.current === token) { operation.current = null; if (mounted.current) setPending(false) } }
  }, [api, accept])

  const dispose = useCallback(async (clear = false) => {
    const previous = current.current
    if (!previous) return
    setError(null)
    const version = ++epoch.current
    const token = Symbol('dispose')
    suspended.current = true
    operation.current = token
    try {
      // DELETE deliberately bypasses the command lock, including a stuck continue.
      if (!disposed.current) await api.dispose(previous.id)
      if (version !== epoch.current || current.current?.id !== previous.id) return
      disposed.current = true
      const next = clear ? null : { ...previous, state: 'cancelled' as const, busy: false }
      current.current = next
      setSession(next)
    } catch (cause) { if (version === epoch.current) { suspended.current = false; setConnectionEpoch(value => value + 1); setError(`Could not cancel worker: ${message(cause)}`) } }
    finally { if (operation.current === token) { operation.current = null; if (mounted.current) setPending(false) } }
  }, [api])

  return { capabilities, session, launchContext, pending, error, transport, launch, send, dispose, refreshCapabilities, upload: api.resource, workspaceBuildInfo: api.workspaceBuildInfo }
}

function message(cause: unknown) { return cause instanceof Error ? cause.message : String(cause) }
export type DebuggerSession = ReturnType<typeof useDebuggerSession>
