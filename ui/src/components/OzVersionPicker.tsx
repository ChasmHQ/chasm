import { useState, useEffect, useRef } from 'react'
import { ChevronDown, Package, Globe, Plus, Loader2, RefreshCw } from 'lucide-react'

interface OzStatus {
    local: boolean
    localPath: string | null
    globalVersions: string[]
    activeVersion: string | null
}

interface OzVersionPickerProps {
    onOpenInstallModal: () => void
    onVersionChanged?: () => void
}

export function OzVersionPicker({ onOpenInstallModal, onVersionChanged }: OzVersionPickerProps) {
    const [status, setStatus] = useState<OzStatus | null>(null)
    const [open, setOpen] = useState(false)
    const [applying, setApplying] = useState(false)
    const dropdownRef = useRef<HTMLDivElement>(null)

    const fetchStatus = async () => {
        try {
            const res = await fetch('/oz/status')
            const data = await res.json()
            setStatus(data)
        } catch {
            // silently ignore — backend may not be ready yet
        }
    }

    useEffect(() => {
        fetchStatus()
    }, [])

    // Close dropdown when clicking outside
    useEffect(() => {
        if (!open) return
        const handler = (e: MouseEvent) => {
            if (dropdownRef.current && !dropdownRef.current.contains(e.target as Node)) {
                setOpen(false)
            }
        }
        document.addEventListener('mousedown', handler)
        return () => document.removeEventListener('mousedown', handler)
    }, [open])

    const applyVersion = async (type: 'global' | 'local', version?: string) => {
        setApplying(true)
        setOpen(false)
        try {
            if (type === 'local') {
                await fetch('/oz/use-local', { method: 'POST' })
            } else if (version) {
                await fetch('/oz/use-global', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ version }),
                })
            }
            await fetchStatus()
            onVersionChanged?.()
        } finally {
            setApplying(false)
        }
    }

    const badgeLabel = () => {
        if (!status || !status.activeVersion) return 'OZ –'
        if (status.activeVersion === 'local') return 'OZ local'
        return `OZ v${status.activeVersion}`
    }

    const isActive = (type: 'global' | 'local', version?: string) => {
        if (!status?.activeVersion) return false
        if (type === 'local') return status.activeVersion === 'local'
        return status.activeVersion === version
    }

    const hasOptions = status && (status.local || status.globalVersions.length > 0)

    return (
        <div className="relative" ref={dropdownRef}>
            <button
                onClick={() => setOpen(o => !o)}
                disabled={applying}
                className="flex items-center gap-1.5 px-2 py-1 rounded text-[10px] font-bold uppercase tracking-wide border border-slate-700 bg-slate-900 hover:bg-slate-800 text-slate-400 hover:text-slate-200 transition-colors disabled:opacity-50"
                title="Switch OpenZeppelin version"
            >
                {applying
                    ? <Loader2 size={10} className="animate-spin" />
                    : <Package size={10} />
                }
                {badgeLabel()}
                <ChevronDown size={10} className={open ? 'rotate-180 transition-transform' : 'transition-transform'} />
            </button>

            {open && (
                <div className="absolute right-0 top-full mt-1 w-56 bg-slate-900 border border-slate-700 rounded-lg shadow-xl z-50 overflow-hidden">
                    <div className="px-3 py-2 border-b border-slate-800">
                        <p className="text-[10px] font-bold uppercase text-slate-500 tracking-wider">OpenZeppelin</p>
                    </div>

                    <div className="py-1 max-h-60 overflow-y-auto">
                        {/* Local option */}
                        {status?.local && (
                            <button
                                onClick={() => applyVersion('local')}
                                className={`w-full flex items-center gap-2 px-3 py-2 text-xs text-left transition-colors ${
                                    isActive('local')
                                        ? 'bg-indigo-900/40 text-indigo-200'
                                        : 'text-slate-300 hover:bg-slate-800'
                                }`}
                            >
                                <Package size={12} className="shrink-0 text-slate-500" />
                                <span className="flex-1">Local <span className="text-slate-600">(lib/)</span></span>
                                {isActive('local') && <span className="text-[9px] text-indigo-400 font-bold">ACTIVE</span>}
                            </button>
                        )}

                        {/* Global versions */}
                        {status?.globalVersions.map(v => (
                            <button
                                key={v}
                                onClick={() => applyVersion('global', v)}
                                className={`w-full flex items-center gap-2 px-3 py-2 text-xs text-left transition-colors ${
                                    isActive('global', v)
                                        ? 'bg-indigo-900/40 text-indigo-200'
                                        : 'text-slate-300 hover:bg-slate-800'
                                }`}
                            >
                                <Globe size={12} className="shrink-0 text-slate-500" />
                                <span className="flex-1 font-mono">v{v}</span>
                                {isActive('global', v) && <span className="text-[9px] text-indigo-400 font-bold">ACTIVE</span>}
                            </button>
                        ))}

                        {!hasOptions && (
                            <p className="px-3 py-2 text-xs text-slate-600 italic">No versions installed.</p>
                        )}
                    </div>

                    <div className="border-t border-slate-800 py-1">
                        <button
                            onClick={() => { setOpen(false); fetchStatus(); }}
                            className="w-full flex items-center gap-2 px-3 py-2 text-xs text-slate-500 hover:text-slate-300 hover:bg-slate-800 transition-colors"
                        >
                            <RefreshCw size={12} className="shrink-0" />
                            Refresh
                        </button>
                        <button
                            onClick={() => { setOpen(false); onOpenInstallModal(); }}
                            className="w-full flex items-center gap-2 px-3 py-2 text-xs text-slate-400 hover:text-slate-200 hover:bg-slate-800 transition-colors"
                        >
                            <Plus size={12} className="shrink-0" />
                            Install new version...
                        </button>
                    </div>
                </div>
            )}
        </div>
    )
}
