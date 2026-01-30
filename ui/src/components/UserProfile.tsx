import { useState, useEffect, useRef } from 'react'
import { Settings, CheckCircle2, XCircle, Copy, Eye, EyeOff, RefreshCw, ChevronUp, Check, Wallet, Key, Plus, Trash2 } from 'lucide-react'
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
    const [balance, setBalance] = useState<string | null>(null)
    const [isRefreshing, setIsRefreshing] = useState(false)
    const [isBalanceRefreshing, setIsBalanceRefreshing] = useState(false)
    const [isCopied, setIsCopied] = useState(false)

    // UI State
    const [mode, setMode] = useState<'raw' | 'keystore'>('raw')
    
    // Raw Mode State
    const [editRpc, setEditRpc] = useState(rpcUrl)
    const [editKey, setEditKey] = useState(privateKey)
    const [showKey, setShowKey] = useState(false)

    // Keystore Mode State
    const [keystores, setKeystores] = useState<string[]>([])
    const [selectedAccount, setSelectedAccount] = useState('')
    const [keystorePassword, setKeystorePassword] = useState('')
    const [isUnlocking, setIsUnlocking] = useState(false)
    const [unlockError, setUnlockError] = useState('')

    // Create/Import Account State
    const [isCreating, setIsCreating] = useState(false)
    const [newAccountName, setNewAccountName] = useState('')
    const [newAccountPassword, setNewAccountPassword] = useState('')
    const [newAccountPrivateKey, setNewAccountPrivateKey] = useState('')
    const [createError, setCreateError] = useState('')

    // Delete State
    const [isDeleting, setIsDeleting] = useState(false)

    const containerRef = useRef<HTMLDivElement>(null)

    useEffect(() => {
        const handleClick = (e: MouseEvent) => {
            if (containerRef.current && !containerRef.current.contains(e.target as Node)) {
                setIsOpen(false)
            }
        }
        document.addEventListener('mousedown', handleClick)
        return () => document.removeEventListener('mousedown', handleClick)
    }, [])

    useEffect(() => {
        if (!isOpen) {
            setEditRpc(rpcUrl)
            setEditKey(privateKey)
            setUnlockError('')
            setCreateError('')
            setKeystorePassword('')
            setIsCreating(false)
            setIsDeleting(false)
        } else {
            if (mode === 'keystore') fetchKeystores()
        }
    }, [isOpen, rpcUrl, privateKey, mode])

    const fetchKeystores = () => {
        fetch('/keystores')
            .then(res => res.json())
            .then(data => {
                if (data.accounts) {
                    setKeystores(data.accounts)
                    if (data.accounts.length > 0 && !selectedAccount) setSelectedAccount(data.accounts[0])
                }
            })
            .catch(err => console.error("Failed to load keystores", err))
    }

    useEffect(() => {
        if (isOpen && mode === 'keystore') fetchKeystores()
    }, [mode, isOpen])

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

    useEffect(() => {
        if (isOpen) fetchBalance()
    }, [publicClient, address, isConnected, isOpen])

    const handleSaveRaw = () => {
        setIsRefreshing(true)
        onUpdateSettings(editRpc, editKey)
        setTimeout(() => {
            setIsRefreshing(false)
            setIsOpen(false)
        }, 500)
    }

    const handleUnlock = async () => {
        if (!selectedAccount || !keystorePassword) return
        setUnlockError('')
        setIsUnlocking(true)
        try {
            const res = await fetch('/keystores/unlock', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ account: selectedAccount, password: keystorePassword })
            })
            const data = await res.json()
            if (data.privateKey) {
                onUpdateSettings(editRpc, data.privateKey)
                setIsOpen(false)
            } else {
                setUnlockError(data.error || 'Failed to unlock')
            }
        } catch (e) {
            setUnlockError('Network error')
        } finally {
            setIsUnlocking(false)
        }
    }

    const handleCreateOrImport = async () => {
        if (!newAccountName || !newAccountPassword || !newAccountPrivateKey) {
            setCreateError('All fields including private key are required')
            return
        }
        setCreateError('')
        setIsUnlocking(true)
        try {
            const res = await fetch('/keystores/create', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ 
                    account: newAccountName, 
                    password: newAccountPassword,
                    privateKey: newAccountPrivateKey.trim()
                })
            })
            const data = await res.json()
            if (data.status === 'success') {
                setIsCreating(false)
                setNewAccountName('')
                setNewAccountPassword('')
                setNewAccountPrivateKey('')
                fetchKeystores()
                setSelectedAccount(newAccountName)
            } else {
                setCreateError(data.error || 'Failed to process request')
            }
        } catch (e) {
            setCreateError('Network error')
        } finally {
            setIsUnlocking(false)
        }
    }

    const handleDelete = async () => {
        if (!selectedAccount || !keystorePassword) {
            setUnlockError('Password required to delete')
            return
        }
        setUnlockError('')
        setIsUnlocking(true)
        try {
            const res = await fetch('/keystores/remove', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ account: selectedAccount, password: keystorePassword })
            })
            const data = await res.json()
            if (data.status === 'success') {
                setIsDeleting(false)
                setKeystorePassword('')
                fetchKeystores()
                // Clear selected account if it was the one deleted
                setSelectedAccount('')
            } else {
                setUnlockError(data.error || 'Failed to delete')
            }
        } catch (e) {
            setUnlockError('Network error')
        } finally {
            setIsUnlocking(false)
        }
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
            >
                <div className="relative">
                    <div className={clsx(
                        "rounded-full bg-gradient-to-tr from-indigo-500 to-purple-500 flex items-center justify-center font-bold text-white",
                        compact ? "w-6 h-6 text-[10px]" : "w-8 h-8 text-xs"
                    )}>
                        {address ? address.slice(2, 4).toUpperCase() : '?'}
                    </div>
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

            {isOpen && (
                <div className={clsx(
                    "absolute bg-slate-900 border border-slate-700 rounded-lg shadow-xl overflow-hidden z-50 w-[320px]",
                    compact ? "left-14 bottom-0 ml-2" : "bottom-full left-2 right-2 mb-2"
                )}>
                    {/* Header */}
                    <div className="p-4 bg-slate-950 border-b border-slate-800 text-center">
                        <div className="text-xs text-slate-500 uppercase tracking-wider mb-1">Current Balance</div>
                        <div className="text-xl font-bold text-slate-200 flex items-center justify-center gap-2">
                            <span>{balance ? parseFloat(balance).toFixed(4) : '0.00'} <span className="text-sm font-normal text-slate-500">ETH</span></span>
                            <button onClick={() => { setIsBalanceRefreshing(true); fetchBalance().then(() => setIsBalanceRefreshing(false)) }} className="text-slate-500 hover:text-slate-300">
                                <RefreshCw size={12} className={isBalanceRefreshing ? "animate-spin" : ""} />
                            </button>
                        </div>
                        <div className={clsx("mt-2 inline-flex items-center gap-1.5 px-3 py-1.5 rounded text-[10px] cursor-pointer transition-all", isCopied ? "bg-green-500/10 text-green-400 border border-green-500/20" : "bg-slate-900 text-slate-400 hover:bg-slate-800")} onClick={handleCopy}>
                            {isCopied ? <><Check size={10} strokeWidth={3} /> Copied!</> : <>{dropdownShortAddress} <Copy size={10} /></>}
                        </div>
                    </div>

                    <div className="p-4 space-y-4">
                        {/* Mode Switch */}
                        <div className="flex bg-slate-950 p-1 rounded-lg border border-slate-800">
                            <button onClick={() => setMode('raw')} className={`flex-1 flex items-center justify-center gap-2 py-1.5 text-[10px] font-medium rounded-md transition-all ${mode === 'raw' ? 'bg-slate-800 text-white shadow' : 'text-slate-500 hover:text-slate-300'}`}>
                                <Key size={12} /> Raw
                            </button>
                            <button onClick={() => setMode('keystore')} className={`flex-1 flex items-center justify-center gap-2 py-1.5 text-[10px] font-medium rounded-md transition-all ${mode === 'keystore' ? 'bg-indigo-900/50 text-indigo-200 shadow border border-indigo-500/30' : 'text-slate-500 hover:text-slate-300'}`}>
                                <Wallet size={12} /> Keystore
                            </button>
                        </div>

                        {/* RPC Input (Common) */}
                        <div className="space-y-1.5">
                            <label className="text-[10px] uppercase font-bold text-slate-500">RPC Network</label>
                            <div className="relative">
                                <input className={clsx("w-full bg-slate-950 border rounded px-2 py-1.5 text-xs text-slate-200 focus:outline-none pr-8", isConnected ? "border-green-900/50 focus:border-green-500/50" : "border-red-900/50 focus:border-red-500/50")} value={editRpc} onChange={(e) => setEditRpc(e.target.value)} />
                                <div className="absolute right-2 top-1/2 -translate-y-1/2">{isConnected ? <CheckCircle2 size={12} className="text-green-500" /> : <XCircle size={12} className="text-red-500" />}</div>
                            </div>
                        </div>

                        {mode === 'raw' ? (
                            <div className="space-y-1.5">
                                <label className="text-[10px] uppercase font-bold text-slate-500">Private Key</label>
                                <div className="relative">
                                    <input type={showKey ? "text" : "password"} className="w-full bg-slate-950 border border-slate-700 rounded px-2 py-1.5 text-xs text-slate-200 focus:outline-none focus:border-indigo-500 pr-8" value={editKey} onChange={(e) => setEditKey(e.target.value)} />
                                    <button onClick={() => setShowKey(!showKey)} className="absolute right-2 top-1/2 -translate-y-1/2 text-slate-500 hover:text-slate-300">
                                        {showKey ? <EyeOff size={12} /> : <Eye size={12} />}
                                    </button>
                                </div>
                                <button onClick={handleSaveRaw} disabled={isRefreshing} className="w-full mt-2 bg-indigo-600 hover:bg-indigo-500 text-white text-xs font-semibold py-2 rounded flex items-center justify-center gap-2 transition-colors">
                                    {isRefreshing ? <RefreshCw size={12} className="animate-spin" /> : <Settings size={12} />} Update
                                </button>
                            </div>
                        ) : (
                            <div className="space-y-3">
                                {isCreating ? (
                                    <div className="bg-slate-950 border border-slate-800 rounded p-3 space-y-2 animate-in fade-in zoom-in-95">
                                        <div className="flex justify-between items-center"><span className="text-[10px] font-bold text-slate-400">CREATE / IMPORT ACCOUNT</span><button onClick={() => setIsCreating(false)} className="text-slate-500 hover:text-slate-300"><XCircle size={12} /></button></div>
                                        <input type="text" placeholder="Account Name (e.g. Sepolia)" className="w-full bg-slate-900 border border-slate-700 rounded px-2 py-1.5 text-xs text-slate-200" value={newAccountName} onChange={e => setNewAccountName(e.target.value)} />
                                        <input type="password" placeholder="Password" className="w-full bg-slate-900 border border-slate-700 rounded px-2 py-1.5 text-xs text-slate-200" value={newAccountPassword} onChange={e => setNewAccountPassword(e.target.value)} />
                                        <div className="border-t border-slate-800 my-1 pt-1">
                                            <label className="text-[9px] text-slate-500 font-bold">PRIVATE KEY</label>
                                            <input type="password" placeholder="Enter private key" className="w-full bg-slate-900 border border-slate-700 rounded px-2 py-1.5 text-xs text-slate-200 mt-1" value={newAccountPrivateKey} onChange={e => setNewAccountPrivateKey(e.target.value)} />
                                        </div>
                                        {createError && <p className="text-[10px] text-red-400">{createError}</p>}
                                        <button onClick={handleCreateOrImport} disabled={isUnlocking} className="w-full bg-green-600 hover:bg-green-500 text-white text-xs font-semibold py-1.5 rounded">{isUnlocking ? 'Processing...' : 'Save to Keystore'}</button>
                                    </div>
                                ) : (
                                    <>
                                        <div className="space-y-1.5">
                                            <div className="flex justify-between items-center">
                                                <label className="text-[10px] uppercase font-bold text-slate-500">Account</label>
                                                <button onClick={() => setIsCreating(true)} className="text-[10px] text-indigo-400 hover:text-indigo-300 flex items-center gap-1"><Plus size={10} /> Create / Import</button>
                                            </div>
                                            <div className="flex gap-2">
                                                <select value={selectedAccount} onChange={(e) => setSelectedAccount(e.target.value)} className="w-full bg-slate-950 border border-slate-700 rounded px-2 py-1.5 text-xs text-slate-200 focus:outline-none">
                                                    {keystores.length === 0 && <option value="">Scanning...</option>}
                                                    {keystores.map(acc => <option key={acc} value={acc}>{acc}</option>)}
                                                    {keystores.length === 0 && <option disabled value="">No accounts found</option>}
                                                </select>
                                                <button onClick={() => setIsDeleting(true)} className="px-2 bg-red-900/20 border border-red-900/50 hover:bg-red-900/40 text-red-400 rounded"><Trash2 size={12} /></button>
                                            </div>
                                        </div>
                                        
                                        {isDeleting ? (
                                            <div className="bg-red-950/30 border border-red-900/50 rounded p-3 space-y-2 animate-in fade-in zoom-in-95">
                                                <p className="text-[10px] text-red-300 font-bold">⚠️ DELETE ACCOUNT PERMANENTLY?</p>
                                                <p className="text-[10px] text-red-400">This action cannot be undone. Please confirm.</p>
                                                <label className="text-[10px] uppercase font-bold text-slate-500">Enter Password to Confirm</label>
                                                <input type="password" className="w-full bg-slate-950 border border-slate-700 rounded px-2 py-1.5 text-xs text-slate-200 focus:outline-none" value={keystorePassword} onChange={(e) => setKeystorePassword(e.target.value)} placeholder="Wallet password" />
                                                <div className="flex gap-2 pt-1">
                                                    <button onClick={() => setIsDeleting(false)} className="flex-1 bg-slate-800 hover:bg-slate-700 text-white text-xs py-1.5 rounded">Cancel</button>
                                                    <button onClick={handleDelete} disabled={isUnlocking} className="flex-1 bg-red-600 hover:bg-red-500 text-white text-xs font-semibold py-1.5 rounded">{isUnlocking ? 'Deleting...' : 'Confirm Delete'}</button>
                                                </div>
                                            </div>
                                        ) : (
                                            <>
                                                <div className="space-y-1.5">
                                                    <label className="text-[10px] uppercase font-bold text-slate-500">Password</label>
                                                    <input type="password" className="w-full bg-slate-950 border border-slate-700 rounded px-2 py-1.5 text-xs text-slate-200 focus:outline-none" value={keystorePassword} onChange={(e) => setKeystorePassword(e.target.value)} onKeyDown={(e) => e.key === 'Enter' && handleUnlock()} placeholder="Enter wallet password" />
                                                </div>
                                                {unlockError && <p className="text-[10px] text-red-400 bg-red-950/30 p-1.5 rounded border border-red-900/50">{unlockError}</p>}
                                                <button onClick={handleUnlock} disabled={isUnlocking} className="w-full bg-indigo-600 hover:bg-indigo-500 text-white text-xs font-semibold py-2 rounded flex items-center justify-center gap-2 transition-colors">
                                                    {isUnlocking ? <RefreshCw size={12} className="animate-spin" /> : <Wallet size={12} />} Unlock & Connect
                                                </button>
                                            </>
                                        )}
                                    </>
                                )}
                            </div>
                        )}
                    </div>
                </div>
            )}
        </div>
    )
}