import { useState, useEffect } from 'react'
import { X, Package, Globe, AlertTriangle, Download, CheckCircle2, Loader2, ExternalLink } from 'lucide-react'

interface OpenZeppelinModalProps {
    isOpen: boolean
    onClose: () => void
    errorMessage: string
}

type Tab = 'local' | 'global'
type InstallState = 'idle' | 'loading' | 'success' | 'error'

const RELEASES_URL = 'https://github.com/OpenZeppelin/openzeppelin-contracts/releases'

// Defined outside the modal so React doesn't remount them on every state change.
// Inline component definitions cause unmount+remount each render, losing input focus.

const ReleasesLink = () => (
    <a
        href={RELEASES_URL}
        target="_blank"
        rel="noopener noreferrer"
        className="inline-flex items-center gap-1 text-xs text-indigo-400 hover:text-indigo-300 transition-colors"
    >
        View all releases <ExternalLink size={10} />
    </a>
)

const VersionInput = ({
    value,
    onChange,
    disabled,
}: { value: string; onChange: (v: string) => void; disabled: boolean }) => (
    <div className="space-y-1.5">
        <label className="text-sm font-medium text-slate-400 block">
            OpenZeppelin Version
        </label>
        <input
            type="text"
            value={value}
            onChange={(e) => onChange(e.target.value)}
            disabled={disabled}
            placeholder="e.g. 4.9.6"
            className="w-full bg-slate-950 border border-slate-700 rounded px-3 py-2 text-sm text-slate-200 focus:border-indigo-500 outline-none disabled:opacity-50 transition-colors"
        />
        <div className="flex items-center justify-between">
            <p className="text-xs text-slate-600">
                Examples: <span className="text-slate-500">4.9.6</span>, <span className="text-slate-500">5.0.2</span>, <span className="text-slate-500">5.1.0</span>
            </p>
            <ReleasesLink />
        </div>
    </div>
)

const StatusRow = ({ state, loadingText, successText, error }: {
    state: InstallState
    loadingText: string
    successText: string
    error: string
}) => (
    <>
        {state === 'loading' && (
            <div className="flex items-center gap-2 text-sm text-slate-400">
                <Loader2 size={14} className="animate-spin shrink-0" />
                {loadingText}
            </div>
        )}
        {state === 'success' && (
            <div className="flex items-center gap-2 text-sm text-emerald-400">
                <CheckCircle2 size={14} className="shrink-0" />
                {successText}
            </div>
        )}
        {state === 'error' && error && (
            <p className="text-xs text-red-400 bg-red-950/30 p-2 rounded border border-red-900/50 font-mono whitespace-pre-wrap break-all">
                {error}
            </p>
        )}
    </>
)

export function OpenZeppelinModal({ isOpen, onClose, errorMessage }: OpenZeppelinModalProps) {
    const [activeTab, setActiveTab] = useState<Tab>('local')

    // Local install state
    const [localVersion, setLocalVersion] = useState('4.9.6')
    const [localInstallState, setLocalInstallState] = useState<InstallState>('idle')
    const [localError, setLocalError] = useState('')

    // Global library state
    const [globalVersions, setGlobalVersions] = useState<string[]>([])
    const [selectedGlobalVersion, setSelectedGlobalVersion] = useState('')
    const [newGlobalVersion, setNewGlobalVersion] = useState('4.9.6')
    const [globalInstallState, setGlobalInstallState] = useState<InstallState>('idle')
    const [globalError, setGlobalError] = useState('')
    const [globalMode, setGlobalMode] = useState<'select' | 'download'>('select')

    useEffect(() => {
        if (isOpen) {
            setActiveTab('local')
            setLocalVersion('4.9.6')
            setLocalInstallState('idle')
            setLocalError('')
            setGlobalInstallState('idle')
            setGlobalError('')
            setNewGlobalVersion('4.9.6')
            fetchGlobalVersions()
        }
    }, [isOpen])

    const fetchGlobalVersions = async () => {
        try {
            const res = await fetch('/oz/global-versions')
            const data = await res.json()
            const versions: string[] = data.versions || []
            setGlobalVersions(versions)
            if (versions.length > 0) {
                setSelectedGlobalVersion(versions[versions.length - 1])
                setGlobalMode('select')
            } else {
                setGlobalMode('download')
            }
        } catch {
            setGlobalVersions([])
            setGlobalMode('download')
        }
    }

    if (!isOpen) return null

    // ── Local Install ──────────────────────────────────────────────────────────

    const handleLocalInstall = async () => {
        setLocalError('')
        setLocalInstallState('loading')
        try {
            const res = await fetch('/oz/install/local', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ version: localVersion }),
            })
            const data = await res.json()
            if (data.error) {
                setLocalInstallState('error')
                setLocalError(data.error)
                return
            }
            setLocalInstallState('success')
            setTimeout(() => onClose(), 1500)
        } catch {
            setLocalInstallState('error')
            setLocalError('Network error. Make sure Chasm is running.')
        }
    }

    // ── Global Library ─────────────────────────────────────────────────────────

    const handleGlobalUse = async () => {
        if (!selectedGlobalVersion) return
        setGlobalError('')
        setGlobalInstallState('loading')
        try {
            const res = await fetch('/oz/use-global', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ version: selectedGlobalVersion }),
            })
            const data = await res.json()
            if (data.error) {
                setGlobalInstallState('error')
                setGlobalError(data.error)
                return
            }
            setGlobalInstallState('success')
            setTimeout(() => onClose(), 1500)
        } catch {
            setGlobalInstallState('error')
            setGlobalError('Network error. Make sure Chasm is running.')
        }
    }

    const handleGlobalDownload = async () => {
        setGlobalError('')
        setGlobalInstallState('loading')
        try {
            const res = await fetch('/oz/install/global', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ version: newGlobalVersion }),
            })
            const data = await res.json()
            if (data.error) {
                setGlobalInstallState('error')
                setGlobalError(data.error)
                return
            }
            setGlobalInstallState('success')
            setTimeout(() => onClose(), 1500)
        } catch {
            setGlobalInstallState('error')
            setGlobalError('Network error. Make sure Chasm is running.')
        }
    }

    const isLoading = localInstallState === 'loading' || globalInstallState === 'loading'

    return (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm">
            <div className="bg-slate-900 border border-slate-700 rounded-lg shadow-2xl w-[520px] overflow-hidden">

                {/* Header */}
                <div className="flex items-center justify-between px-4 py-3 border-b border-slate-800 bg-slate-950">
                    <div className="flex items-center gap-2">
                        <AlertTriangle size={16} className="text-amber-400" />
                        <h3 className="font-semibold text-slate-200">OpenZeppelin Not Found</h3>
                    </div>
                    <button
                        onClick={onClose}
                        disabled={isLoading}
                        className="text-slate-500 hover:text-slate-200 transition-colors disabled:opacity-40"
                    >
                        <X size={18} />
                    </button>
                </div>

                {/* Error context */}
                <div className="px-4 pt-4">
                    <p className="text-sm text-slate-400 mb-1">
                        Import <code className="text-amber-300 bg-amber-950/30 px-1 rounded">@openzeppelin</code> not found. Choose how to install:
                    </p>
                    {errorMessage && (
                        <p className="text-xs text-slate-600 font-mono truncate" title={errorMessage}>
                            {errorMessage.slice(0, 120)}{errorMessage.length > 120 ? '…' : ''}
                        </p>
                    )}
                </div>

                {/* Tab switcher */}
                <div className="flex mx-4 mt-4 bg-slate-950 p-1 rounded-lg border border-slate-800">
                    <button
                        onClick={() => setActiveTab('local')}
                        className={`flex-1 flex items-center justify-center gap-2 py-1.5 text-sm font-medium rounded-md transition-all ${
                            activeTab === 'local'
                                ? 'bg-slate-800 text-white shadow'
                                : 'text-slate-500 hover:text-slate-300'
                        }`}
                    >
                        <Package size={14} /> Local Install
                    </button>
                    <button
                        onClick={() => setActiveTab('global')}
                        className={`flex-1 flex items-center justify-center gap-2 py-1.5 text-sm font-medium rounded-md transition-all ${
                            activeTab === 'global'
                                ? 'bg-indigo-900/50 text-indigo-200 shadow border border-indigo-500/30'
                                : 'text-slate-500 hover:text-slate-300'
                        }`}
                    >
                        <Globe size={14} /> Global Library
                    </button>
                </div>

                {/* Tab Content */}
                <div className="p-4 space-y-4 min-h-[220px]">

                    {/* ── Local Install Tab ── */}
                    {activeTab === 'local' && (
                        <div className="space-y-4">
                            <p className="text-xs text-slate-500">
                                Clone OpenZeppelin directly into your project's{' '}
                                <code className="text-slate-400">lib/openzeppelin-contracts</code> folder using git.
                                No git repository required.
                            </p>
                            <VersionInput
                                value={localVersion}
                                onChange={setLocalVersion}
                                disabled={isLoading}
                            />
                            <StatusRow
                                state={localInstallState}
                                loadingText="Cloning via git... this may take a moment"
                                successText="Done! Recompiling..."
                                error={localError}
                            />
                        </div>
                    )}

                    {/* ── Global Library Tab ── */}
                    {activeTab === 'global' && (
                        <div className="space-y-4">
                            <p className="text-xs text-slate-500">
                                Use a shared library managed by Chasm at{' '}
                                <code className="text-slate-400">~/.chasm/lib/</code>.
                                Install once, reuse across all projects.
                            </p>

                            {/* Existing versions */}
                            {globalVersions.length > 0 && (
                                <div className="space-y-2">
                                    <label className="text-sm font-medium text-slate-400 block">
                                        Available versions
                                    </label>
                                    <div className="space-y-1">
                                        {globalVersions.map((v) => (
                                            <button
                                                key={v}
                                                onClick={() => {
                                                    setSelectedGlobalVersion(v)
                                                    setGlobalMode('select')
                                                }}
                                                className={`w-full text-left px-3 py-2 rounded text-sm transition-colors border ${
                                                    selectedGlobalVersion === v && globalMode === 'select'
                                                        ? 'bg-indigo-900/40 border-indigo-500/50 text-indigo-200'
                                                        : 'border-slate-700/50 text-slate-400 hover:bg-slate-800 hover:text-slate-200'
                                                }`}
                                            >
                                                <span className="font-mono">v{v}</span>
                                                <span className="text-xs text-slate-600 ml-2">
                                                    openzeppelin-contracts@{v}
                                                </span>
                                            </button>
                                        ))}
                                    </div>
                                </div>
                            )}

                            {/* Download new version */}
                            <div className={`space-y-2 ${globalVersions.length > 0 ? 'border-t border-slate-800 pt-3' : ''}`}>
                                {globalVersions.length > 0 && (
                                    <p className="text-xs text-slate-500 font-medium">Or download a new version:</p>
                                )}
                                <VersionInput
                                    value={newGlobalVersion}
                                    onChange={(v) => {
                                        setNewGlobalVersion(v)
                                        setGlobalMode('download')
                                    }}
                                    disabled={isLoading}
                                />
                                {globalMode === 'download' && newGlobalVersion && (
                                    <p className="text-xs text-slate-600">
                                        Will be cloned to{' '}
                                        <code className="text-slate-500">
                                            ~/.chasm/lib/openzeppelin-contracts@{newGlobalVersion}/
                                        </code>
                                    </p>
                                )}
                            </div>

                            <StatusRow
                                state={globalInstallState}
                                loadingText={globalMode === 'download' ? 'Cloning via git... this may take a moment' : 'Applying remapping...'}
                                successText="Done! Recompiling..."
                                error={globalError}
                            />
                        </div>
                    )}
                </div>

                {/* Footer */}
                <div className="px-4 py-3 border-t border-slate-800 bg-slate-950 flex justify-end gap-3">
                    <button
                        onClick={onClose}
                        disabled={isLoading}
                        className="px-4 py-2 text-sm font-medium text-slate-400 hover:text-slate-200 hover:bg-slate-800 rounded transition-colors disabled:opacity-40"
                    >
                        Cancel
                    </button>

                    {activeTab === 'local' && (
                        <button
                            onClick={handleLocalInstall}
                            disabled={isLoading || !localVersion.trim() || localInstallState === 'success'}
                            className="px-4 py-2 text-sm font-medium bg-indigo-600 hover:bg-indigo-500 disabled:opacity-50 disabled:cursor-not-allowed text-white rounded flex items-center gap-2 transition-colors"
                        >
                            {localInstallState === 'loading' ? (
                                <><Loader2 size={14} className="animate-spin" /> Cloning...</>
                            ) : (
                                <><Download size={14} /> Install & Compile</>
                            )}
                        </button>
                    )}

                    {activeTab === 'global' && (
                        <button
                            onClick={globalMode === 'select' ? handleGlobalUse : handleGlobalDownload}
                            disabled={
                                isLoading ||
                                globalInstallState === 'success' ||
                                (globalMode === 'select' && !selectedGlobalVersion) ||
                                (globalMode === 'download' && !newGlobalVersion.trim())
                            }
                            className="px-4 py-2 text-sm font-medium bg-indigo-600 hover:bg-indigo-500 disabled:opacity-50 disabled:cursor-not-allowed text-white rounded flex items-center gap-2 transition-colors"
                        >
                            {globalInstallState === 'loading' ? (
                                <><Loader2 size={14} className="animate-spin" /> {globalMode === 'download' ? 'Cloning...' : 'Applying...'}</>
                            ) : globalMode === 'download' ? (
                                <><Download size={14} /> Download & Compile</>
                            ) : (
                                <><Globe size={14} /> Use v{selectedGlobalVersion}</>
                            )}
                        </button>
                    )}
                </div>
            </div>
        </div>
    )
}
