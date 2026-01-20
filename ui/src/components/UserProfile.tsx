import { useState, useEffect, useRef } from 'react'
import { Settings, CheckCircle2, XCircle, Copy, Eye, EyeOff, RefreshCw, ChevronUp, Check } from 'lucide-react'
import { clsx } from 'clsx'
import type { Address, PublicClient } from 'viem'
import { formatEther } from 'viem'

interface UserProfileProps {
    address: Address | undefined;
    rpcUrl: string;
    privateKey: string;
    isConnected: boolean;
    publicClient: PublicClient | null;
    onUpdateSettings: (rpcUrl: string, privateKey: string) => void;
    compact?: boolean;
}

export function UserProfile({ 
    address, 
    rpcUrl, 
    privateKey, 
    isConnected, 
    publicClient,
    onUpdateSettings,
    compact = false
}: UserProfileProps) {
    const [isOpen, setIsOpen] = useState(false)
    const [editRpc, setEditRpc] = useState(rpcUrl)
    const [editKey, setEditKey] = useState(privateKey)
    const [showKey, setShowKey] = useState(false)
    const [balance, setBalance] = useState<string | null>(null)
    const [isRefreshing, setIsRefreshing] = useState(false)
    const [isCopied, setIsCopied] = useState(false)

    const containerRef = useRef<HTMLDivElement>(null)

    // Close on click outside
    useEffect(() => {
        const handleClick = (e: MouseEvent) => {
            if (containerRef.current && !containerRef.current.contains(e.target as Node)) {
                setIsOpen(false)
            }
        }
        document.addEventListener('mousedown', handleClick)
        return () => document.removeEventListener('mousedown', handleClick)
    }, [])

    // Sync props to local state when closed
    useEffect(() => {
        if (!isOpen) {
            setEditRpc(rpcUrl)
            setEditKey(privateKey)
        }
    }, [isOpen, rpcUrl, privateKey])

    // Fetch Balance
    useEffect(() => {
        const fetchBalance = async () => {
            if (publicClient && address && isConnected) {
                try {
                    const bal = await publicClient.getBalance({ address })
                    setBalance(formatEther(bal))
                } catch (e) {
                    console.error("Failed to fetch balance", e)
                }
            }
        }
        
        if (isOpen) fetchBalance()
        
        // Poll balance if connected
        const interval = setInterval(fetchBalance, 5000)
        return () => clearInterval(interval)
    }, [publicClient, address, isConnected, isOpen])

    const handleSave = () => {
        setIsRefreshing(true)
        onUpdateSettings(editRpc, editKey)
        // Simulate loading feedback
        setTimeout(() => setIsRefreshing(false), 500)
    }

    const handleCopy = () => {
        if (address) {
            navigator.clipboard.writeText(address)
            setIsCopied(true)
            setTimeout(() => setIsCopied(false), 2000)
        }
    }

    const shortAddress = address ? `${address.slice(0, 6)}...${address.slice(-4)}` : 'No Account'
    const dropdownShortAddress = address ? `${address.slice(0, 10)}...${address.slice(-8)}` : 'No Account'

    return (
        <div ref={containerRef} className="relative">
            {/* Trigger Button (Sidebar Footer Style) */}
            <button 
                onClick={() => setIsOpen(!isOpen)}
                className={clsx(
                    "flex items-center transition-colors group relative",
                    compact 
                        ? "justify-center w-10 h-10 rounded-lg hover:bg-slate-800" 
                        : "w-full gap-3 p-3 hover:bg-slate-800 border-t border-slate-800 text-left",
                    isOpen && !compact ? "bg-slate-800" : "",
                    isOpen && compact ? "bg-slate-800 text-white" : ""
                )}
                title={compact ? (isConnected ? "Connected" : "Disconnected") : undefined}
            >
                {/* Avatar / Icon */}
                <div className="relative">
                    <div className={clsx(
                        "rounded-full bg-gradient-to-tr from-indigo-500 to-purple-500 flex items-center justify-center font-bold text-white",
                        compact ? "w-6 h-6 text-[10px]" : "w-8 h-8 text-xs"
                    )}>
                        {address ? address.slice(2, 4).toUpperCase() : '?'}
                    </div>
                    {/* Status Dot Badge */}
                    <div className={clsx(
                        "absolute -bottom-0.5 -right-0.5 rounded-full border-2 border-slate-900",
                        isConnected ? "bg-green-500" : "bg-red-500",
                        compact ? "w-2.5 h-2.5" : "w-3 h-3"
                    )} />
                </div>

                {!compact && (
                    <>
                        <div className="flex-1 min-w-0">
                            <div className="text-xs font-medium text-slate-200 truncate">{shortAddress}</div>
                            <div className="text-[10px] text-slate-500 truncate flex items-center gap-1">
                                {isConnected ? "RPC Connected" : "RPC Disconnected"}
                            </div>
                        </div>

                        <ChevronUp size={14} className={clsx("text-slate-500 transition-transform", isOpen ? "rotate-180" : "")} />
                    </>
                )}
            </button>

            {/* Dropdown Menu (Popover) */}
            {isOpen && (
                <div className={clsx(
                    "absolute bg-slate-900 border border-slate-700 rounded-lg shadow-xl overflow-hidden z-50 w-[280px]",
                    compact ? "left-14 bottom-0 ml-2" : "bottom-full left-2 right-2 mb-2"
                )}>
                    {/* Header: Balance & Copy */}
                    <div className="p-4 bg-slate-950 border-b border-slate-800 text-center">
                        <div className="text-xs text-slate-500 uppercase tracking-wider mb-1">Current Balance</div>
                        <div className="text-xl font-bold text-slate-200 flex items-center justify-center gap-1">
                            {balance ? parseFloat(balance).toFixed(4) : '0.00'} <span className="text-sm font-normal text-slate-500">ETH</span>
                        </div>
                        <div 
                            className={clsx(
                                "mt-2 inline-flex items-center gap-1.5 px-3 py-1.5 rounded text-[10px] cursor-pointer transition-all duration-200",
                                isCopied 
                                    ? "bg-green-500/10 text-green-400 border border-green-500/20" 
                                    : "bg-slate-900 text-slate-400 hover:bg-slate-800 hover:text-slate-200 border border-transparent"
                            )}
                            onClick={handleCopy}
                            title="Copy Full Address"
                        >
                            {isCopied ? (
                                <>
                                    <Check size={10} strokeWidth={3} /> Copied!
                                </>
                            ) : (
                                <>
                                    {dropdownShortAddress} <Copy size={10} />
                                </>
                            )}
                        </div>
                    </div>

                    {/* Settings Form */}
                    <div className="p-4 space-y-4">
                        <div className="space-y-1.5">
                            <label className="text-[10px] uppercase font-bold text-slate-500">RPC Network</label>
                            <div className="relative">
                                <input 
                                    className={clsx(
                                        "w-full bg-slate-950 border rounded px-2 py-1.5 text-xs text-slate-200 focus:outline-none pr-8",
                                        isConnected ? "border-green-900/50 focus:border-green-500/50" : "border-red-900/50 focus:border-red-500/50"
                                    )}
                                    value={editRpc}
                                    onChange={(e) => setEditRpc(e.target.value)}
                                />
                                <div className="absolute right-2 top-1/2 -translate-y-1/2">
                                    {isConnected 
                                        ? <CheckCircle2 size={12} className="text-green-500" />
                                        : <XCircle size={12} className="text-red-500" />
                                    }
                                </div>
                            </div>
                        </div>

                        <div className="space-y-1.5">
                            <label className="text-[10px] uppercase font-bold text-slate-500">Private Key</label>
                            <div className="relative">
                                <input 
                                    type={showKey ? "text" : "password"}
                                    className="w-full bg-slate-950 border border-slate-700 rounded px-2 py-1.5 text-xs text-slate-200 focus:outline-none focus:border-indigo-500 pr-8"
                                    value={editKey}
                                    onChange={(e) => setEditKey(e.target.value)}
                                />
                                <button 
                                    onClick={() => setShowKey(!showKey)}
                                    className="absolute right-2 top-1/2 -translate-y-1/2 text-slate-500 hover:text-slate-300"
                                >
                                    {showKey ? <EyeOff size={12} /> : <Eye size={12} />}
                                </button>
                            </div>
                        </div>

                        <button 
                            onClick={handleSave}
                            disabled={isRefreshing}
                            className="w-full bg-indigo-600 hover:bg-indigo-500 text-white text-xs font-semibold py-2 rounded flex items-center justify-center gap-2 transition-colors"
                        >
                            {isRefreshing ? <RefreshCw size={12} className="animate-spin" /> : <Settings size={12} />}
                            Update Configuration
                        </button>
                    </div>
                </div>
            )}
        </div>
    )
}