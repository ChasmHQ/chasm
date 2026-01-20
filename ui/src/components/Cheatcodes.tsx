import { useState } from 'react'
import { clsx } from 'clsx'
import { Terminal, Clock, Box, Wallet, Shield, Hash, ChevronDown } from 'lucide-react'
import type { PublicClient, TestClient, Address, Hex } from 'viem'
import { parseUnits, numberToHex } from 'viem'

interface CheatcodesProps {
    testClient?: TestClient;
    publicClient: PublicClient;
    onLog: (msg: string) => void;
    enabled: boolean;
    embedded?: boolean;
    mode?: 'live' | 'queue';
    queuedCount?: number;
    onQueueAction?: (action: any) => void;
    onClearQueue?: () => void;
    queuedActions?: any[];
    onUpdateQueue?: (next: any[]) => void;
}

export function Cheatcodes({ testClient, publicClient, onLog, enabled, embedded = false, mode = 'live', queuedCount = 0, onQueueAction, onClearQueue, queuedActions = [], onUpdateQueue }: CheatcodesProps) {
    const [activeSection, setActiveSection] = useState<'env' | 'account' | 'state'>('env')
    const [editingIndex, setEditingIndex] = useState<number | null>(null)
    
    // Environment State
    const [warpTime, setWarpTime] = useState("")
    const [rollBlock, setRollBlock] = useState("")
    
    // Account State
    const [dealAddr, setDealAddr] = useState("")
    const [dealAmount, setDealAmount] = useState("")
    const [dealUnit, setDealUnit] = useState<'wei' | 'gwei' | 'ether'>('ether')
    const [isDealUnitOpen, setIsDealUnitOpen] = useState(false)
    const [nonceAddr, setNonceAddr] = useState("")
    const [nonceVal, setNonceVal] = useState("")
    const [etchAddr, setEtchAddr] = useState("")
    const [etchCode, setEtchCode] = useState("")
    const [storageAddr, setStorageAddr] = useState("")
    const [storageSlot, setStorageSlot] = useState("")
    const [storageValue, setStorageValue] = useState("")
    const [prankAddr, setPrankAddr] = useState("")
    const [isPranking, setIsPranking] = useState(false)
    
    const [loading, setLoading] = useState(false)

    // Actions
    const handleWarp = async () => {
        if (!warpTime) return
        if (mode === 'queue') {
            const action = { type: 'warp', value: warpTime }
            if (editingIndex !== null) {
                const next = [...queuedActions]
                next[editingIndex] = action
                onUpdateQueue?.(next)
                setEditingIndex(null)
            } else {
                onQueueAction?.(action)
            }
            onLog(`Queued warp: ${warpTime}`)
            return
        }
        if (!testClient) {
            onLog("Test client unavailable")
            return
        }
        setLoading(true)
        try {
            // Check if relative or absolute
            let target = BigInt(warpTime)
            if (warpTime.startsWith("+")) {
                const current = await publicClient.getBlock().then(b => b.timestamp)
                target = current + BigInt(warpTime.slice(1))
            }
            await testClient.setNextBlockTimestamp({ timestamp: target })
            await testClient.mine({ blocks: 1 })
            onLog(`Warped to timestamp ${target}`)
        } catch(e: any) { onLog(`Error: ${e.message}`) }
        finally { setLoading(false) }
    }

    const handleRoll = async () => {
        if (!rollBlock) return
        if (mode === 'queue') {
            const action = { type: 'roll', value: rollBlock }
            if (editingIndex !== null) {
                const next = [...queuedActions]
                next[editingIndex] = action
                onUpdateQueue?.(next)
                setEditingIndex(null)
            } else {
                onQueueAction?.(action)
            }
            onLog(`Queued roll: ${rollBlock}`)
            return
        }
        if (!testClient) {
            onLog("Test client unavailable")
            return
        }
        setLoading(true)
        try {
            let target = BigInt(rollBlock)
            const current = await publicClient.getBlockNumber()
            
            if (rollBlock.startsWith("+")) {
                target = current + BigInt(rollBlock.slice(1))
            }
            
            if (target <= current) {
                 throw new Error(`Target block ${target} must be greater than current ${current}`)
            }

            const diff = Number(target - current)
            // Limit diff to prevent hanging
            if (diff > 1000) {
                 if (!window.confirm(`You are about to mine ${diff} blocks. This might take a moment. Continue?`)) {
                     return
                 }
            }
            
            await testClient.mine({ blocks: diff })
            onLog(`Rolled (Mined) ${diff} blocks to ${target}`)
        } catch(e: any) { onLog(`Error: ${e.message}`) }
        finally { setLoading(false) }
    }

    const handleDeal = async () => {
        if (!dealAddr || !dealAmount) return
        if (mode === 'queue') {
            const action = { type: 'deal', address: dealAddr, amount: dealAmount, unit: dealUnit }
            if (editingIndex !== null) {
                const next = [...queuedActions]
                next[editingIndex] = action
                onUpdateQueue?.(next)
                setEditingIndex(null)
            } else {
                onQueueAction?.(action)
            }
            onLog(`Queued deal: ${dealAddr}`)
            return
        }
        if (!testClient) {
            onLog("Test client unavailable")
            return
        }
        setLoading(true)
        try {
            const val = parseUnits(dealAmount, dealUnit === 'wei' ? 0 : dealUnit === 'gwei' ? 9 : 18)
            await testClient.setBalance({ address: dealAddr as Address, value: val })
            onLog(`Set balance of ${dealAddr} to ${dealAmount} ${dealUnit === 'ether' ? 'ETH' : dealUnit}`)
        } catch(e: any) { onLog(`Error: ${e.message}`) }
        finally { setLoading(false) }
    }

    const handleNonce = async () => {
        if (!nonceAddr || !nonceVal) return
        if (mode === 'queue') {
            const action = { type: 'nonce', address: nonceAddr, value: nonceVal }
            if (editingIndex !== null) {
                const next = [...queuedActions]
                next[editingIndex] = action
                onUpdateQueue?.(next)
                setEditingIndex(null)
            } else {
                onQueueAction?.(action)
            }
            onLog(`Queued nonce: ${nonceAddr}`)
            return
        }
        if (!testClient) {
            onLog("Test client unavailable")
            return
        }
        setLoading(true)
        try {
            await testClient.setNonce({ address: nonceAddr as Address, nonce: Number(nonceVal) })
            onLog(`Set nonce of ${nonceAddr} to ${nonceVal}`)
        } catch(e: any) { onLog(`Error: ${e.message}`) }
        finally { setLoading(false) }
    }

    const handleEtch = async () => {
        if (!etchAddr || !etchCode) return
        if (mode === 'queue') {
            const action = { type: 'etch', address: etchAddr, bytecode: etchCode }
            if (editingIndex !== null) {
                const next = [...queuedActions]
                next[editingIndex] = action
                onUpdateQueue?.(next)
                setEditingIndex(null)
            } else {
                onQueueAction?.(action)
            }
            onLog(`Queued etch: ${etchAddr}`)
            return
        }
        if (!testClient) {
            onLog("Test client unavailable")
            return
        }
        setLoading(true)
        try {
            await testClient.setCode({ address: etchAddr as Address, bytecode: etchCode as Hex })
            onLog(`Etched code to ${etchAddr}`)
        } catch(e: any) { onLog(`Error: ${e.message}`) }
        finally { setLoading(false) }
    }

    const handleStorage = async () => {
        if (!storageAddr || !storageSlot || !storageValue) return
        if (mode === 'queue') {
            const action = { type: 'storage', address: storageAddr, slot: storageSlot, value: storageValue }
            if (editingIndex !== null) {
                const next = [...queuedActions]
                next[editingIndex] = action
                onUpdateQueue?.(next)
                setEditingIndex(null)
            } else {
                onQueueAction?.(action)
            }
            onLog(`Queued storage: ${storageAddr}`)
            return
        }
        if (!testClient) {
            onLog("Test client unavailable")
            return
        }
        setLoading(true)
        try {
            const slot = storageSlot.startsWith('0x') ? storageSlot as Hex : numberToHex(BigInt(storageSlot), { size: 32 })
            const val = storageValue.startsWith('0x') ? storageValue as Hex : numberToHex(BigInt(storageValue), { size: 32 })
            await testClient.setStorageAt({ address: storageAddr as Address, index: slot, value: val })
            onLog(`Set storage at ${storageAddr} slot ${slot}`)
        } catch(e: any) { onLog(`Error: ${e.message}`) }
        finally { setLoading(false) }
    }

    const handlePrank = async () => {
        if (!prankAddr) return
        if (mode === 'queue') {
            const action = { type: 'prank', address: prankAddr }
            if (editingIndex !== null) {
                const next = [...queuedActions]
                next[editingIndex] = action
                onUpdateQueue?.(next)
                setEditingIndex(null)
            } else {
                onQueueAction?.(action)
            }
            onLog(`Queued prank: ${prankAddr}`)
            return
        }
        if (!testClient) {
            onLog("Test client unavailable")
            return
        }
        setLoading(true)
        try {
            await testClient.impersonateAccount({ address: prankAddr as Address })
            setIsPranking(true)
            onLog(`Pranking as ${prankAddr}`)
        } catch(e: any) { onLog(`Error: ${e.message}`) }
        finally { setLoading(false) }
    }

    const handleStopPrank = async () => {
        if (!prankAddr) return
        if (mode === 'queue') {
            const action = { type: 'stopPrank', address: prankAddr }
            if (editingIndex !== null) {
                const next = [...queuedActions]
                next[editingIndex] = action
                onUpdateQueue?.(next)
                setEditingIndex(null)
            } else {
                onQueueAction?.(action)
            }
            onLog(`Queued stop prank: ${prankAddr}`)
            return
        }
        if (!testClient) {
            onLog("Test client unavailable")
            return
        }
        setLoading(true)
        try {
            await testClient.stopImpersonatingAccount({ address: prankAddr as Address })
            setIsPranking(false)
            onLog(`Stopped pranking ${prankAddr}`)
        } catch(e: any) { onLog(`Error: ${e.message}`) }
        finally { setLoading(false) }
    }

    const renderQueuedLabel = (action: any) => {
        if (!action) return "Unknown action"
        switch (action.type) {
            case "warp": return `Warp ${action.value}`
            case "roll": return `Roll ${action.value}`
            case "deal": return `Deal ${action.address} ${action.amount} ${action.unit === 'ether' ? 'ETH' : action.unit || 'ETH'}`
            case "nonce": return `Nonce ${action.address} -> ${action.value}`
            case "etch": return `Etch ${action.address}`
            case "storage": return `Storage ${action.address} ${action.slot}`
            case "prank": return `Prank ${action.address}`
            case "stopPrank": return `Stop Prank ${action.address}`
            default: return action.type || "Unknown action"
        }
    }

    return (
        <div className={clsx("flex flex-col h-full bg-slate-950 text-slate-200", embedded && "h-[360px]")}>
            {!embedded && (
                <div className="flex items-center px-4 py-2 bg-slate-900/50 border-b border-slate-800 h-[45px]">
                    <div className="flex items-center gap-2">
                        <h3 className="text-xs font-bold text-slate-500 uppercase tracking-wider flex items-center gap-2">
                            <Terminal size={14} /> Foundry Cheatcodes
                        </h3>
                    </div>
                </div>
            )}

            {!enabled && (
                <div className="px-4 py-2 border-b border-slate-800 bg-slate-900/60 text-xs text-amber-300">
                    Enable Test mode in a call/deploy tab to use cheatcodes.
                </div>
            )}

            {mode === 'queue' && (
                <div className="px-4 py-2 border-b border-slate-800 bg-slate-900/60 text-xs text-slate-400 flex items-center justify-between">
                    <span>Queued actions: {queuedCount}</span>
                    <button onClick={onClearQueue} className="text-indigo-300 hover:text-indigo-200 text-[10px] font-bold uppercase">Clear</button>
                </div>
            )}

            {mode === 'queue' && queuedActions.length > 0 && (
                <div className="px-4 py-3 border-b border-slate-800 bg-slate-900/40 text-xs text-slate-300 space-y-2">
                    {queuedActions.map((action, idx) => (
                        <div key={`${action.type}-${idx}`} className="flex items-center justify-between gap-2 bg-slate-900/60 border border-slate-800 rounded px-2 py-2">
                            <span className="truncate">{renderQueuedLabel(action)}</span>
                            <div className="flex items-center gap-2 shrink-0">
                                <button
                                    onClick={() => {
                                        if (action.type === "warp") setWarpTime(action.value || "")
                                        if (action.type === "roll") setRollBlock(action.value || "")
                                        if (action.type === "deal") {
                                            setDealAddr(action.address || "")
                                            setDealAmount(action.amount || "")
                                            setDealUnit(action.unit || "ether")
                                        }
                                        if (action.type === "nonce") {
                                            setNonceAddr(action.address || "")
                                            setNonceVal(action.value || "")
                                        }
                                        if (action.type === "etch") {
                                            setEtchAddr(action.address || "")
                                            setEtchCode(action.bytecode || "")
                                        }
                                        if (action.type === "storage") {
                                            setStorageAddr(action.address || "")
                                            setStorageSlot(action.slot || "")
                                            setStorageValue(action.value || "")
                                        }
                                        if (action.type === "prank" || action.type === "stopPrank") {
                                            setPrankAddr(action.address || "")
                                        }
                                        setEditingIndex(idx)
                                    }}
                                    className="text-[10px] uppercase font-bold text-indigo-300 hover:text-indigo-200"
                                >
                                    Edit
                                </button>
                                <button
                                    onClick={() => onUpdateQueue?.(queuedActions.filter((_, i) => i !== idx))}
                                    className="text-[10px] uppercase font-bold text-red-300 hover:text-red-200"
                                >
                                    Delete
                                </button>
                            </div>
                        </div>
                    ))}
                </div>
            )}

            <div className={clsx("flex flex-1 overflow-hidden", !enabled && "opacity-50 pointer-events-none")}>
                {embedded ? (
                    <div className="flex-1 flex flex-col overflow-hidden">
                        <div className="flex items-center gap-2 px-4 py-2 border-b border-slate-800 bg-slate-900/40">
                            <button onClick={() => setActiveSection('env')} className={clsx("px-3 py-1.5 rounded-md text-xs font-semibold transition-colors", activeSection === 'env' ? "bg-slate-800 text-indigo-300" : "text-slate-500 hover:text-slate-300 hover:bg-slate-800/50")}>
                                <Clock size={14} className="inline mr-1" /> Environment
                            </button>
                            <button onClick={() => setActiveSection('account')} className={clsx("px-3 py-1.5 rounded-md text-xs font-semibold transition-colors", activeSection === 'account' ? "bg-slate-800 text-indigo-300" : "text-slate-500 hover:text-slate-300 hover:bg-slate-800/50")}>
                                <Wallet size={14} className="inline mr-1" /> Account
                            </button>
                        </div>
                        <div className="flex-1 overflow-y-auto p-6">
                            <div className="max-w-2xl mx-auto space-y-8">
                                {activeSection === 'env' && (
                                    <div className="space-y-6 animate-in fade-in slide-in-from-bottom-2 duration-300">
                                        <div className="space-y-4">
                                            <div className="flex items-center gap-2 text-indigo-400 border-b border-indigo-500/20 pb-2">
                                                <Clock size={18} />
                                                <h3 className="font-bold text-sm uppercase tracking-wider">Time Travel (Warp)</h3>
                                            </div>
                                            <div className="flex gap-2">
                                                <input 
                                                    className="flex-1 bg-slate-900 border border-slate-800 rounded px-3 py-2 text-sm focus:border-indigo-500 outline-none placeholder:text-slate-600"
                                                    placeholder="Timestamp or +Seconds (e.g. +3600)"
                                                    value={warpTime}
                                                    onChange={e => setWarpTime(e.target.value)}
                                                />
                                                <button onClick={handleWarp} disabled={loading} className="px-4 py-2 bg-indigo-600 hover:bg-indigo-500 text-white rounded text-sm font-bold disabled:opacity-50">Warp</button>
                                            </div>
                                            <p className="text-xs text-slate-500">Sets the block.timestamp for the next block.</p>
                                        </div>

                                        <div className="space-y-4">
                                            <div className="flex items-center gap-2 text-indigo-400 border-b border-indigo-500/20 pb-2">
                                                <Box size={18} />
                                                <h3 className="font-bold text-sm uppercase tracking-wider">Block Travel (Roll)</h3>
                                            </div>
                                            <div className="flex gap-2">
                                                <input 
                                                    className="flex-1 bg-slate-900 border border-slate-800 rounded px-3 py-2 text-sm focus:border-indigo-500 outline-none placeholder:text-slate-600"
                                                    placeholder="Block Number or +Blocks (e.g. +10)"
                                                    value={rollBlock}
                                                    onChange={e => setRollBlock(e.target.value)}
                                                />
                                                <button onClick={handleRoll} disabled={loading} className="px-4 py-2 bg-indigo-600 hover:bg-indigo-500 text-white rounded text-sm font-bold disabled:opacity-50">Roll</button>
                                            </div>
                                            <p className="text-xs text-slate-500">Sets the block.number for the next block.</p>
                                        </div>
                                    </div>
                                )}

                                {activeSection === 'account' && (
                                    <div className="space-y-8 animate-in fade-in slide-in-from-bottom-2 duration-300">
                                        <div className="space-y-4">
                                            <div className="flex items-center gap-2 text-indigo-400 border-b border-indigo-500/20 pb-2">
                                                <Wallet size={18} />
                                                <h3 className="font-bold text-sm uppercase tracking-wider">Deal (Set Balance)</h3>
                                            </div>
                                            <div className="grid grid-cols-1 md:grid-cols-3 gap-2">
                                                <input 
                                                    className="md:col-span-2 bg-slate-900 border border-slate-800 rounded px-3 py-2 text-sm focus:border-indigo-500 outline-none placeholder:text-slate-600"
                                                    placeholder="Address (0x...)"
                                                    value={dealAddr}
                                                    onChange={e => setDealAddr(e.target.value)}
                                                />
                                                <div className="relative flex gap-2 md:col-span-1">
                                                    <input 
                                                        className="w-full bg-slate-900 border border-slate-800 rounded px-3 py-2 text-sm focus:border-indigo-500 outline-none placeholder:text-slate-600"
                                                        placeholder="Amount"
                                                        value={dealAmount}
                                                        onChange={e => setDealAmount(e.target.value)}
                                                    />
                                                    <button
                                                        onClick={() => setIsDealUnitOpen(!isDealUnitOpen)}
                                                        className="bg-slate-900 border border-slate-800 text-xs text-slate-400 focus:outline-none focus:text-slate-200 cursor-pointer flex items-center gap-1 px-2 hover:bg-slate-800 transition-colors shrink-0"
                                                    >
                                                        {dealUnit === 'ether' ? 'ETH' : dealUnit} <ChevronDown size={10} />
                                                    </button>
                                                    {isDealUnitOpen && (
                                                        <div className="absolute top-full right-0 mt-1 bg-slate-900 border border-slate-700 rounded shadow-lg z-50 w-20 overflow-hidden">
                                                            {['ether', 'gwei', 'wei'].map((unit) => (
                                                                <div
                                                                    key={unit}
                                                                    onClick={() => {
                                                                        setDealUnit(unit as any)
                                                                        setIsDealUnitOpen(false)
                                                                    }}
                                                                    className={clsx(
                                                                        "px-3 py-1.5 text-xs cursor-pointer hover:bg-slate-800 transition-colors uppercase",
                                                                        dealUnit === unit
                                                                            ? "text-indigo-400 font-bold bg-slate-800/50"
                                                                            : "text-slate-400",
                                                                    )}
                                                                >
                                                                    {unit === 'ether' ? 'ETH' : unit}
                                                                </div>
                                                            ))}
                                                        </div>
                                                    )}
                                                </div>
                                            </div>
                                            <button onClick={handleDeal} disabled={loading} className="w-full py-2 bg-indigo-600 hover:bg-indigo-500 text-white rounded text-sm font-bold disabled:opacity-50">Set Balance</button>
                                        </div>

                                        <div className="space-y-4">
                                            <div className="flex items-center gap-2 text-indigo-400 border-b border-indigo-500/20 pb-2">
                                                <Shield size={18} />
                                                <h3 className="font-bold text-sm uppercase tracking-wider">Prank (Impersonate)</h3>
                                            </div>
                                            <div className="flex gap-2">
                                                <input 
                                                    className="flex-1 bg-slate-900 border border-slate-800 rounded px-3 py-2 text-sm focus:border-indigo-500 outline-none placeholder:text-slate-600"
                                                    placeholder="Address (0x...)"
                                                    value={prankAddr}
                                                    onChange={e => setPrankAddr(e.target.value)}
                                                />
                                                {!isPranking ? (
                                                    <button onClick={handlePrank} disabled={loading} className="px-4 py-2 bg-indigo-600 hover:bg-indigo-500 text-white rounded text-sm font-bold disabled:opacity-50">Prank</button>
                                                ) : (
                                                    <button onClick={handleStopPrank} disabled={loading} className="px-4 py-2 bg-slate-700 hover:bg-slate-600 text-white rounded text-sm font-bold disabled:opacity-50">Stop</button>
                                                )}
                                            </div>
                                            <p className="text-xs text-slate-500">Impersonate an address for subsequent calls.</p>
                                        </div>
                                    </div>
                                )}
                            </div>
                        </div>
                    </div>
                ) : (
                    <>
                        {/* Sidebar Navigation */}
                        <div className="w-48 border-r border-slate-800 bg-slate-900/30 p-2 space-y-1">
                            <button onClick={() => setActiveSection('env')} className={clsx("w-full flex items-center gap-3 px-3 py-2 rounded-md text-sm font-medium transition-colors", activeSection === 'env' ? "bg-slate-800 text-indigo-400" : "text-slate-500 hover:text-slate-300 hover:bg-slate-800/50")}>
                                <Clock size={16} /> Environment
                            </button>
                            <button onClick={() => setActiveSection('account')} className={clsx("w-full flex items-center gap-3 px-3 py-2 rounded-md text-sm font-medium transition-colors", activeSection === 'account' ? "bg-slate-800 text-indigo-400" : "text-slate-500 hover:text-slate-300 hover:bg-slate-800/50")}>
                                <Wallet size={16} /> Account
                            </button>
                        </div>

                        {/* Content Area */}
                        <div className="flex-1 overflow-y-auto p-8">
                    <div className="max-w-2xl mx-auto space-y-8">
                        
                        {activeSection === 'env' && (
                            <div className="space-y-6 animate-in fade-in slide-in-from-bottom-2 duration-300">
                                <div className="space-y-4">
                                    <div className="flex items-center gap-2 text-indigo-400 border-b border-indigo-500/20 pb-2">
                                        <Clock size={18} />
                                        <h3 className="font-bold text-sm uppercase tracking-wider">Time Travel (Warp)</h3>
                                    </div>
                                    <div className="flex gap-2">
                                        <input 
                                            className="flex-1 bg-slate-900 border border-slate-800 rounded px-3 py-2 text-sm focus:border-indigo-500 outline-none placeholder:text-slate-600"
                                            placeholder="Timestamp or +Seconds (e.g. +3600)"
                                            value={warpTime}
                                            onChange={e => setWarpTime(e.target.value)}
                                        />
                                        <button onClick={handleWarp} disabled={loading} className="px-4 py-2 bg-indigo-600 hover:bg-indigo-500 text-white rounded text-sm font-bold disabled:opacity-50">Warp</button>
                                    </div>
                                    <p className="text-xs text-slate-500">Sets the block.timestamp for the next block.</p>
                                </div>

                                <div className="space-y-4">
                                    <div className="flex items-center gap-2 text-indigo-400 border-b border-indigo-500/20 pb-2">
                                        <Box size={18} />
                                        <h3 className="font-bold text-sm uppercase tracking-wider">Block Travel (Roll)</h3>
                                    </div>
                                    <div className="flex gap-2">
                                        <input 
                                            className="flex-1 bg-slate-900 border border-slate-800 rounded px-3 py-2 text-sm focus:border-indigo-500 outline-none placeholder:text-slate-600"
                                            placeholder="Block Number or +Blocks (e.g. +10)"
                                            value={rollBlock}
                                            onChange={e => setRollBlock(e.target.value)}
                                        />
                                        <button onClick={handleRoll} disabled={loading} className="px-4 py-2 bg-indigo-600 hover:bg-indigo-500 text-white rounded text-sm font-bold disabled:opacity-50">Roll</button>
                                    </div>
                                    <p className="text-xs text-slate-500">Sets the block.number for the next block.</p>
                                </div>
                            </div>
                        )}

                        {activeSection === 'account' && (
                            <div className="space-y-8 animate-in fade-in slide-in-from-bottom-2 duration-300">
                                <div className="space-y-4">
                                    <div className="flex items-center gap-2 text-indigo-400 border-b border-indigo-500/20 pb-2">
                                        <Wallet size={18} />
                                        <h3 className="font-bold text-sm uppercase tracking-wider">Deal (Set Balance)</h3>
                                    </div>
                                    <div className="grid grid-cols-1 md:grid-cols-3 gap-2">
                                        <input 
                                            className="md:col-span-2 bg-slate-900 border border-slate-800 rounded px-3 py-2 text-sm focus:border-indigo-500 outline-none placeholder:text-slate-600"
                                            placeholder="Address (0x...)"
                                            value={dealAddr}
                                            onChange={e => setDealAddr(e.target.value)}
                                        />
                                        <div className="relative flex gap-2 md:col-span-1">
                                            <input 
                                                className="w-full bg-slate-900 border border-slate-800 rounded px-3 py-2 text-sm focus:border-indigo-500 outline-none placeholder:text-slate-600"
                                                placeholder="Amount"
                                                value={dealAmount}
                                                onChange={e => setDealAmount(e.target.value)}
                                            />
                                            <button
                                                onClick={() => setIsDealUnitOpen(!isDealUnitOpen)}
                                                className="bg-slate-900 border border-slate-800 text-xs text-slate-400 focus:outline-none focus:text-slate-200 cursor-pointer flex items-center gap-1 px-2 hover:bg-slate-800 transition-colors shrink-0"
                                            >
                                                {dealUnit === 'ether' ? 'ETH' : dealUnit} <ChevronDown size={10} />
                                            </button>
                                            {isDealUnitOpen && (
                                                <div className="absolute top-full right-0 mt-1 bg-slate-900 border border-slate-700 rounded shadow-lg z-50 w-20 overflow-hidden">
                                                    {['ether', 'gwei', 'wei'].map((unit) => (
                                                        <div
                                                            key={unit}
                                                            onClick={() => {
                                                                setDealUnit(unit as any)
                                                                setIsDealUnitOpen(false)
                                                            }}
                                                            className={clsx(
                                                                "px-3 py-1.5 text-xs cursor-pointer hover:bg-slate-800 transition-colors uppercase",
                                                                dealUnit === unit
                                                                    ? "text-indigo-400 font-bold bg-slate-800/50"
                                                                    : "text-slate-400",
                                                            )}
                                                        >
                                                            {unit === 'ether' ? 'ETH' : unit}
                                                        </div>
                                                    ))}
                                                </div>
                                            )}
                                        </div>
                                    </div>
                                    <button onClick={handleDeal} disabled={loading} className="w-full py-2 bg-indigo-600 hover:bg-indigo-500 text-white rounded text-sm font-bold disabled:opacity-50">Set Balance</button>
                                </div>

                                <div className="space-y-4">
                                    <div className="flex items-center gap-2 text-indigo-400 border-b border-indigo-500/20 pb-2">
                                        <Shield size={18} />
                                        <h3 className="font-bold text-sm uppercase tracking-wider">Prank (Impersonate)</h3>
                                    </div>
                                    <div className="flex gap-2">
                                        <input 
                                            className="flex-1 bg-slate-900 border border-slate-800 rounded px-3 py-2 text-sm focus:border-indigo-500 outline-none placeholder:text-slate-600"
                                            placeholder="Address (0x...)"
                                            value={prankAddr}
                                            onChange={e => setPrankAddr(e.target.value)}
                                        />
                                        {!isPranking ? (
                                            <button onClick={handlePrank} disabled={loading} className="px-4 py-2 bg-indigo-600 hover:bg-indigo-500 text-white rounded text-sm font-bold disabled:opacity-50">Prank</button>
                                        ) : (
                                            <button onClick={handleStopPrank} disabled={loading} className="px-4 py-2 bg-slate-700 hover:bg-slate-600 text-white rounded text-sm font-bold disabled:opacity-50">Stop</button>
                                        )}
                                    </div>
                                    <p className="text-xs text-slate-500">Impersonate an address for subsequent calls.</p>
                                </div>

                                <div className="space-y-4">
                                    <div className="flex items-center gap-2 text-indigo-400 border-b border-indigo-500/20 pb-2">
                                        <Hash size={18} />
                                        <h3 className="font-bold text-sm uppercase tracking-wider">Set Nonce</h3>
                                    </div>
                                    <div className="grid grid-cols-3 gap-2">
                                        <input 
                                            className="col-span-2 bg-slate-900 border border-slate-800 rounded px-3 py-2 text-sm focus:border-indigo-500 outline-none placeholder:text-slate-600"
                                            placeholder="Address (0x...)"
                                            value={nonceAddr}
                                            onChange={e => setNonceAddr(e.target.value)}
                                        />
                                        <input 
                                            className="bg-slate-900 border border-slate-800 rounded px-3 py-2 text-sm focus:border-indigo-500 outline-none placeholder:text-slate-600"
                                            placeholder="Nonce"
                                            type="number"
                                            value={nonceVal}
                                            onChange={e => setNonceVal(e.target.value)}
                                        />
                                    </div>
                                    <button onClick={handleNonce} disabled={loading} className="w-full py-2 bg-indigo-600 hover:bg-indigo-500 text-white rounded text-sm font-bold disabled:opacity-50">Set Nonce</button>
                                </div>

                                <div className="space-y-4">
                                    <div className="flex items-center gap-2 text-indigo-400 border-b border-indigo-500/20 pb-2">
                                        <Terminal size={18} />
                                        <h3 className="font-bold text-sm uppercase tracking-wider">Etch (Set Code)</h3>
                                    </div>
                                    <input 
                                        className="w-full bg-slate-900 border border-slate-800 rounded px-3 py-2 text-sm focus:border-indigo-500 outline-none placeholder:text-slate-600"
                                        placeholder="Address (0x...)"
                                        value={etchAddr}
                                        onChange={e => setEtchAddr(e.target.value)}
                                    />
                                    <textarea 
                                        className="w-full h-24 bg-slate-900 border border-slate-800 rounded px-3 py-2 text-sm focus:border-indigo-500 outline-none placeholder:text-slate-600 font-mono resize-none"
                                        placeholder="Bytecode (0x...)"
                                        value={etchCode}
                                        onChange={e => setEtchCode(e.target.value)}
                                    />
                                    <button onClick={handleEtch} disabled={loading} className="w-full py-2 bg-indigo-600 hover:bg-indigo-500 text-white rounded text-sm font-bold disabled:opacity-50">Etch Code</button>
                                </div>

                                <div className="space-y-4">
                                    <div className="flex items-center gap-2 text-indigo-400 border-b border-indigo-500/20 pb-2">
                                        <Shield size={18} />
                                        <h3 className="font-bold text-sm uppercase tracking-wider">Set Storage</h3>
                                    </div>
                                    <input 
                                        className="w-full bg-slate-900 border border-slate-800 rounded px-3 py-2 text-sm focus:border-indigo-500 outline-none placeholder:text-slate-600"
                                        placeholder="Address (0x...)"
                                        value={storageAddr}
                                        onChange={e => setStorageAddr(e.target.value)}
                                    />
                                    <div className="grid grid-cols-2 gap-2">
                                        <input 
                                            className="bg-slate-900 border border-slate-800 rounded px-3 py-2 text-sm focus:border-indigo-500 outline-none placeholder:text-slate-600"
                                            placeholder="Slot (0x... or int)"
                                            value={storageSlot}
                                            onChange={e => setStorageSlot(e.target.value)}
                                        />
                                        <input 
                                            className="bg-slate-900 border border-slate-800 rounded px-3 py-2 text-sm focus:border-indigo-500 outline-none placeholder:text-slate-600"
                                            placeholder="Value (0x... or int)"
                                            value={storageValue}
                                            onChange={e => setStorageValue(e.target.value)}
                                        />
                                    </div>
                                    <button onClick={handleStorage} disabled={loading} className="w-full py-2 bg-indigo-600 hover:bg-indigo-500 text-white rounded text-sm font-bold disabled:opacity-50">Set Storage</button>
                                </div>
                            </div>
                        )}

                    </div>
                </div>
                    </>
                )}
            </div>
        </div>
    )
}
