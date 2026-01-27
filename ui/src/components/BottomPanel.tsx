import { useRef, useEffect, useMemo, useState } from 'react'
import { Terminal, Trash2 } from 'lucide-react'
import { clsx } from 'clsx'

interface BottomPanelProps {
    logs: { message: string; timestamp: string }[];
    onClear: () => void;
    snapshots: {
        id: string;
        snapshotId: string;
        createdAt: number;
        method: string;
        from?: string;
        to?: string;
        value?: string;
        txHash?: string;
        blockNumber?: number;
        status: 'pending' | 'confirmed' | 'error';
    }[];
    activeSnapshotId: string | null;
    onRevert: (snapshotId: string) => void;
}

export function BottomPanel({ logs, onClear, snapshots, activeSnapshotId, onRevert }: BottomPanelProps) {
    const scrollRef = useRef<HTMLDivElement>(null)
    const [activeTab, setActiveTab] = useState<'logs' | 'snapshots'>('logs')

    useEffect(() => {
        if (scrollRef.current) {
            scrollRef.current.scrollTop = scrollRef.current.scrollHeight
        }
    }, [logs])

    const sortedSnapshots = useMemo(() => snapshots.slice().sort((a, b) => b.createdAt - a.createdAt), [snapshots])
    const activeIndex = useMemo(
        () => sortedSnapshots.findIndex((snap) => snap.snapshotId === activeSnapshotId),
        [sortedSnapshots, activeSnapshotId],
    )

    return (
        <div className="flex flex-col h-full bg-slate-900 border-t border-slate-800">
             <div className="flex items-center justify-between px-4 py-2 bg-slate-950 border-b border-slate-800">
                <div className="flex items-center gap-2 text-slate-400 text-xs font-semibold uppercase tracking-wider">
                    <Terminal size={14} />
                    <button
                        onClick={() => setActiveTab('logs')}
                        className={clsx("px-2 py-1 rounded", activeTab === 'logs' ? "bg-slate-800 text-slate-200" : "text-slate-500")}
                    >
                        Logs & Traces
                    </button>
                    <button
                        onClick={() => setActiveTab('snapshots')}
                        className={clsx("px-2 py-1 rounded", activeTab === 'snapshots' ? "bg-slate-800 text-slate-200" : "text-slate-500")}
                    >
                        Snapshots
                    </button>
                </div>
                <button onClick={onClear} className="text-slate-500 hover:text-slate-300 transition-colors">
                    <Trash2 size={14} />
                </button>
            </div>
            {activeTab === 'logs' ? (
                <div ref={scrollRef} className="flex-1 overflow-y-auto p-4 font-mono text-xs text-slate-400 space-y-1">
                    {logs.length === 0 && <div className="opacity-30 italic">No logs yet.</div>}
                    {logs.map((log, i) => (
                        <div key={i} className="break-all border-b border-slate-800/30 pb-1 last:border-0 hover:bg-slate-800/20 px-1 rounded">
                            <span className="text-slate-600 mr-2">{log.timestamp}</span>
                            {log.message}
                        </div>
                    ))}
                </div>
            ) : (
                <div className="flex-1 overflow-y-auto p-4 text-xs text-slate-300">
                    {sortedSnapshots.length === 0 && <div className="opacity-30 italic">No snapshots yet.</div>}
                    <div className="relative">
                        <div className="absolute left-3 top-0 bottom-0 w-px bg-slate-800" />
                        <div className="space-y-6">
                            {sortedSnapshots.map((snap, index) => {
                                const isActive = snap.snapshotId === activeSnapshotId
                                const isAhead = activeIndex !== -1 && index < activeIndex
                                return (
                                    <div key={snap.id} className={clsx("flex items-start gap-4", isAhead && "opacity-50")}>
                                        <div className="relative z-10 mt-1">
                                            <div className={clsx(
                                                "h-3 w-3 rounded-full border",
                                                isActive ? "bg-indigo-500 border-indigo-300" : "bg-slate-800 border-slate-600"
                                            )} />
                                            {isActive && (
                                                <span className="absolute left-5 -top-1 text-[9px] uppercase font-bold text-indigo-300">
                                                    HEAD
                                                </span>
                                            )}
                                        </div>
                                        <div className="flex-1 bg-slate-950/50 border border-slate-800 rounded p-3">
                                            <div className="flex items-center justify-between">
                                                <div className="text-[10px] uppercase text-slate-500">
                                                    {new Date(snap.createdAt).toLocaleTimeString()}
                                                </div>
                                                <div className="flex items-center gap-2">
                                                    {isActive && (
                                                        <span className="text-[10px] uppercase font-semibold text-indigo-300">Current</span>
                                                    )}
                                                    <div className={clsx(
                                                        "text-[10px] uppercase font-semibold",
                                                        snap.status === 'confirmed' ? "text-emerald-400" : snap.status === 'error' ? "text-red-400" : "text-slate-400"
                                                    )}>
                                                        {snap.status}
                                                    </div>
                                                </div>
                                            </div>
                                            <div className="mt-2 text-sm font-semibold text-slate-100">
                                                {snap.method}
                                            </div>
                                            <div className="mt-1 grid grid-cols-2 gap-2 text-[10px] text-slate-400">
                                                <div>From: <span className="text-slate-300">{snap.from || '-'}</span></div>
                                                <div>To: <span className="text-slate-300">{snap.to || '-'}</span></div>
                                                <div>Value: <span className="text-slate-300">{snap.value || '0'}</span></div>
                                                <div>Block: <span className="text-slate-300">{snap.blockNumber ?? '-'}</span></div>
                                            </div>
                                            {snap.txHash && (
                                                <div className="mt-2 text-[10px] text-slate-500">
                                                    Tx: <span className="text-slate-300">{snap.txHash}</span>
                                                </div>
                                            )}
                                            <div className="mt-3 flex items-center justify-end">
                                                <button
                                                    onClick={() => onRevert(snap.snapshotId)}
                                                    disabled={isActive || isAhead}
                                                    className={clsx(
                                                        "text-[10px] uppercase font-bold px-3 py-1 rounded border",
                                                        (isActive || isAhead)
                                                            ? "border-slate-800 text-slate-500 cursor-not-allowed"
                                                            : "border-slate-700 text-slate-300 hover:text-white hover:bg-slate-800",
                                                    )}
                                                >
                                                    Revert
                                                </button>
                                            </div>
                                        </div>
                                    </div>
                                )
                            })}
                        </div>
                    </div>
                </div>
            )}
        </div>
    )
}
