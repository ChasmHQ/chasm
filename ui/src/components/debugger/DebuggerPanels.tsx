import { useState } from 'react'
import { display, record, rows, safeStringify } from '../../types/debugger'
import type { DebugRecord, DebugValue, DebuggerSnapshot } from '../../types/debugger'
import { buttonClass, inputClass } from './DebuggerLaunch'

type Pane = 'stack' | 'memory' | 'storage' | 'calldata' | 'logs' | 'gas' | 'diff'
export function DebuggerPanels({ snapshot, disabled, refresh }: { snapshot: DebuggerSnapshot; disabled: boolean; refresh: (params: DebugRecord) => void }) {
  const [pane, setPane] = useState<Pane>('stack')
  const [memoryOffset, setMemoryOffset] = useState('0')
  const [storageSlot, setStorageSlot] = useState('0')
  const [error, setError] = useState('')
  function window() {
    try {
      const integer = BigInt(pane === 'memory' ? memoryOffset : storageSlot)
      if (integer < 0n || (pane === 'memory' ? integer > 16777216n : integer >= 1n << 256n)) throw new Error('Offset is outside the supported window range.')
      setError('')
      refresh(pane === 'memory' ? { memoryOffset: integer.toString() } : { storageSlots: [integer.toString()] })
    } catch (cause) { setError(cause instanceof Error ? cause.message : 'Enter an integer.') }
  }
  const value = snapshot.panes[pane]
  const object = record(value)
  const items = rows(value, 'items', 'logs')
  return <section className="flex h-[240px] shrink-0 flex-col overflow-hidden border-t border-slate-800 bg-slate-950" aria-label="EVM inspection">
    <div role="tablist" aria-label="Inspection panes" className="flex shrink-0 overflow-x-auto border-b border-slate-800 bg-slate-900/50">{(['stack', 'memory', 'storage', 'calldata', 'logs', 'gas', 'diff'] as const).map(name => <button role="tab" aria-selected={pane === name} key={name} onClick={() => setPane(name)} className={`border-b-2 px-4 py-2.5 text-xs capitalize ${pane === name ? 'border-indigo-400 text-indigo-300' : 'border-transparent text-slate-400 hover:text-slate-200'}`}>{name}</button>)}</div>
    {(pane === 'memory' || pane === 'storage') && <div className="flex flex-wrap items-center gap-2 border-b border-slate-800 px-3 py-2 text-xs text-slate-400"><label className="flex items-center gap-2">{pane === 'memory' ? 'Byte offset' : 'Raw slot'}<input aria-label={pane === 'memory' ? 'Memory offset' : 'Storage slot'} className={`${inputClass} !w-36 !py-1`} value={pane === 'memory' ? memoryOffset : storageSlot} onChange={event => pane === 'memory' ? setMemoryOffset(event.target.value) : setStorageSlot(event.target.value)} /></label><button className={buttonClass} disabled={disabled} onClick={window}>Read window</button><span>Bounded snapshot reads</span>{error && <span role="alert" className="text-red-400">{error}</span>}</div>}
    <div role="tabpanel" aria-label={pane} className="min-h-0 flex-1 overflow-auto p-3 font-mono text-xs">
      {value === undefined || value === null ? <Empty text="No data available at this stop." /> : object.error ? <Empty text={display(object.error)} /> : pane === 'stack' ? <DataTable columns={['index', 'hex', 'decimal']} data={items} /> : pane === 'memory' ? <><p className="mb-2 text-slate-500">Allocated {display(object.memory_size)} bytes · window from {display(object.offset)}{object.truncated ? ' · more available' : ''}</p><DataTable columns={['offset', 'hex', 'region', 'beyond']} data={items} /></> : pane === 'storage' ? <><p className="mb-2 break-all text-slate-500">Address {display(object.address)}</p><DataTable columns={['slot', 'name', 'type', 'value', 'hex']} data={items} /></> : pane === 'calldata' ? <><KeyValues value={{ signature: object.signature ?? null, selector: object.selector ?? null, total_bytes: object.total ?? null, offset: object.offset ?? null, truncated: object.truncated ?? false }} /><div className="mt-3 break-all rounded border border-slate-800 p-3 leading-6 text-indigo-200">{display(object.hex)}</div></> : pane === 'logs' ? <DataTable columns={['event', 'address', 'topics', 'data']} data={items} /> : pane === 'gas' ? <><h3 className="mb-2 text-slate-400">Current gas</h3><KeyValues value={record(snapshot.status.gas)} /><h3 className="mb-2 mt-4 text-slate-400">By opcode</h3><KeyValues value={record(object.by_opcode)} /><h3 className="mb-2 mt-4 text-slate-400">By source line</h3><DataTable columns={['file', 'line', 'gas', 'text']} data={rows(object.by_source_line)} /></> : <StructuredValue value={value} />}
      {value !== undefined && <details className="mt-4 text-slate-500"><summary className="cursor-pointer text-[10px]">Structured JSON fallback</summary><pre className="mt-2 whitespace-pre-wrap break-all">{safeStringify(value, 2)}</pre></details>}
    </div>
  </section>
}

export function KeyValues({ value }: { value: DebugRecord }) {
  const entries = Object.entries(value).filter(([, item]) => item !== null)
  return entries.length ? <dl className="space-y-1.5">{entries.map(([key, item]) => <div className="grid grid-cols-[minmax(80px,30%)_1fr] gap-3 border-b border-slate-800/50 pb-1.5" key={key}><dt className="break-words text-slate-500">{key.replaceAll('_', ' ')}</dt><dd className="min-w-0 break-all text-slate-200">{typeof item === 'object' ? <StructuredValue value={item} /> : display(item)}</dd></div>)}</dl> : <Empty />
}

export function StructuredValue({ value }: { value: DebugValue | undefined }) {
  if (value === undefined || value === null) return <Empty />
  if (Array.isArray(value)) return value.length ? <div className="space-y-2">{value.map((item, index) => <div key={index} className="border-l border-slate-700 pl-2"><span className="mr-2 text-slate-500">#{index}</span>{typeof item === 'object' && item !== null ? <StructuredValue value={item} /> : display(item)}</div>)}</div> : <Empty />
  if (typeof value === 'object') return <KeyValues value={value} />
  return <span className="break-all">{display(value)}</span>
}

export function DataTable({ columns, data }: { columns: string[]; data: DebugRecord[] }) {
  const visible = columns.filter(column => data.some(row => row[column] !== undefined && row[column] !== null))
  if (!data.length) return <Empty text="No entries at this stop." />
  return <table className="w-full text-left text-xs"><thead className="sticky top-0 bg-slate-950 text-slate-500"><tr>{visible.map(column => <th className="whitespace-nowrap px-2 py-1.5 font-medium capitalize" key={column}>{column}</th>)}</tr></thead><tbody>{data.map((row, index) => <tr className="border-t border-slate-800/60 hover:bg-slate-900/60" key={index}>{visible.map(column => <td className={`max-w-[560px] px-2 py-2 align-top ${column === 'hex' ? 'whitespace-nowrap text-indigo-200' : 'break-all text-slate-300'}`} key={column}>{Array.isArray(row[column]) ? (row[column] as DebugValue[]).map(display).join('\n') : display(row[column])}</td>)}</tr>)}</tbody></table>
}
export function Empty({ text = 'None' }: { text?: string }) { return <p className="py-2 text-xs text-slate-500">{text}</p> }
