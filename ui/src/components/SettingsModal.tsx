import { useState, useEffect } from 'react'
import { X, Save, Eye, EyeOff, Wallet, Key } from 'lucide-react'

interface SettingsModalProps {
    isOpen: boolean;
    onClose: () => void;
    currentRpcUrl: string;
    currentPrivateKey: string;
    onSave: (rpcUrl: string, privateKey: string) => void;
}

export function SettingsModal({ isOpen, onClose, currentRpcUrl, currentPrivateKey, onSave }: SettingsModalProps) {
    const [rpcUrl, setRpcUrl] = useState(currentRpcUrl)
    const [privateKey, setPrivateKey] = useState(currentPrivateKey)
    const [showKey, setShowKey] = useState(false)
    
    // New States for Keystore
    const [mode, setMode] = useState<'raw' | 'keystore'>('raw')
    const [keystores, setKeystores] = useState<string[]>([])
    const [selectedAccount, setSelectedAccount] = useState('')
    const [keystorePassword, setKeystorePassword] = useState('')
    const [isUnlocking, setIsUnlocking] = useState(false)
    const [unlockError, setUnlockError] = useState('')

    // Reset local state when modal opens
    useEffect(() => {
        if (isOpen) {
            setRpcUrl(currentRpcUrl)
            setPrivateKey(currentPrivateKey)
            // Default to raw mode initially, or detect? keeping raw for now
            setMode('raw') 
            setUnlockError('')
            setKeystorePassword('')
        }
    }, [isOpen, currentRpcUrl, currentPrivateKey])

    // Load keystores when switching to keystore mode
    useEffect(() => {
        if (isOpen && mode === 'keystore') {
            fetch('/keystores')
                .then(res => res.json())
                .then(data => {
                    if (data.accounts) {
                        setKeystores(data.accounts)
                        if (data.accounts.length > 0) setSelectedAccount(data.accounts[0])
                    }
                })
                .catch(err => console.error("Failed to load keystores", err))
        }
    }, [isOpen, mode])

    if (!isOpen) return null

    const handleUnlockAndSave = async () => {
        setUnlockError('')
        
        if (mode === 'raw') {
            onSave(rpcUrl, privateKey)
            onClose()
            return
        }

        // Keystore Logic
        if (!selectedAccount) {
            setUnlockError('Please select an account')
            return
        }
        if (!keystorePassword) {
            setUnlockError('Please enter password')
            return
        }

        setIsUnlocking(true)
        try {
            const res = await fetch('/keystores/unlock', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ account: selectedAccount, password: keystorePassword })
            })
            const data = await res.json()
            
            if (data.privateKey) {
                // Success! Save with the unlocked key
                onSave(rpcUrl, data.privateKey)
                onClose()
            } else {
                setUnlockError(data.error || 'Failed to unlock wallet')
            }
        } catch (e) {
            setUnlockError('Network error during unlock')
        } finally {
            setIsUnlocking(false)
        }
    }

    return (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 backdrop-blur-sm">
            <div className="bg-slate-900 border border-slate-700 rounded-lg shadow-2xl w-[500px] overflow-hidden">
                <div className="flex items-center justify-between px-4 py-3 border-b border-slate-800 bg-slate-950">
                    <h3 className="font-semibold text-slate-200">Environment Settings</h3>
                    <button onClick={onClose} className="text-slate-500 hover:text-slate-200 transition-colors">
                        <X size={18} />
                    </button>
                </div>
                
                <div className="p-6 space-y-6">
                    {/* RPC URL */}
                    <div className="space-y-2">
                        <label className="text-sm font-medium text-slate-400">RPC URL</label>
                        <input 
                            type="text"
                            value={rpcUrl}
                            onChange={(e) => setRpcUrl(e.target.value)}
                            className="w-full bg-slate-950 border border-slate-700 rounded px-3 py-2 text-sm text-slate-200 focus:border-indigo-500 outline-none"
                            placeholder="http://127.0.0.1:8545"
                        />
                    </div>

                    {/* Mode Switcher */}
                    <div className="flex bg-slate-950 p-1 rounded-lg border border-slate-800">
                        <button
                            onClick={() => setMode('raw')}
                            className={`flex-1 flex items-center justify-center gap-2 py-1.5 text-sm font-medium rounded-md transition-all ${mode === 'raw' ? 'bg-slate-800 text-white shadow' : 'text-slate-500 hover:text-slate-300'}`}
                        >
                            <Key size={14} /> Raw Private Key
                        </button>
                        <button
                            onClick={() => setMode('keystore')}
                            className={`flex-1 flex items-center justify-center gap-2 py-1.5 text-sm font-medium rounded-md transition-all ${mode === 'keystore' ? 'bg-indigo-900/50 text-indigo-200 shadow border border-indigo-500/30' : 'text-slate-500 hover:text-slate-300'}`}
                        >
                            <Wallet size={14} /> Foundry Keystore
                        </button>
                    </div>

                    {mode === 'raw' ? (
                        /* Raw Key Input */
                        <div className="space-y-2">
                            <label className="text-sm font-medium text-slate-400">Private Key</label>
                            <div className="relative">
                                <input 
                                    type={showKey ? "text" : "password"}
                                    value={privateKey}
                                    onChange={(e) => setPrivateKey(e.target.value)}
                                    className="w-full bg-slate-950 border border-slate-700 rounded px-3 py-2 text-sm text-slate-200 focus:border-indigo-500 outline-none pr-10"
                                    placeholder="0x..."
                                />
                                <button 
                                    onClick={() => setShowKey(!showKey)}
                                    className="absolute right-3 top-1/2 -translate-y-1/2 text-slate-500 hover:text-slate-300"
                                >
                                    {showKey ? <EyeOff size={16} /> : <Eye size={16} />}
                                </button>
                            </div>
                            <p className="text-xs text-slate-600">Enter a raw private key directly (less secure).</p>
                        </div>
                    ) : (
                        /* Keystore Inputs */
                        <div className="space-y-4 animate-in fade-in zoom-in-95 duration-200">
                            <div className="space-y-2">
                                <label className="text-sm font-medium text-slate-400">Select Account</label>
                                <select 
                                    value={selectedAccount}
                                    onChange={(e) => setSelectedAccount(e.target.value)}
                                    className="w-full bg-slate-950 border border-slate-700 rounded px-3 py-2 text-sm text-slate-200 focus:border-indigo-500 outline-none"
                                >
                                    {keystores.length === 0 && <option value="">Scanning for keystores...</option>}
                                    {keystores.map(acc => (
                                        <option key={acc} value={acc}>{acc}</option>
                                    ))}
                                    {keystores.length === 0 && <option disabled value="">No keystores found in ~/.foundry</option>}
                                </select>
                            </div>
                            <div className="space-y-2">
                                <label className="text-sm font-medium text-slate-400">Password</label>
                                <input 
                                    type="password"
                                    value={keystorePassword}
                                    onChange={(e) => setKeystorePassword(e.target.value)}
                                    onKeyDown={(e) => e.key === 'Enter' && handleUnlockAndSave()}
                                    className="w-full bg-slate-950 border border-slate-700 rounded px-3 py-2 text-sm text-slate-200 focus:border-indigo-500 outline-none"
                                    placeholder="Enter wallet password..."
                                />
                            </div>
                            {unlockError && (
                                <p className="text-xs text-red-400 bg-red-950/30 p-2 rounded border border-red-900/50">{unlockError}</p>
                            )}
                        </div>
                    )}
                </div>

                <div className="px-4 py-3 border-t border-slate-800 bg-slate-950 flex justify-end gap-3">
                    <button 
                        onClick={onClose}
                        className="px-4 py-2 text-sm font-medium text-slate-400 hover:text-slate-200 hover:bg-slate-800 rounded transition-colors"
                    >
                        Cancel
                    </button>
                    <button 
                        onClick={handleUnlockAndSave}
                        disabled={isUnlocking}
                        className="px-4 py-2 text-sm font-medium bg-indigo-600 hover:bg-indigo-500 disabled:opacity-50 disabled:cursor-not-allowed text-white rounded flex items-center gap-2 transition-colors"
                    >
                        {isUnlocking ? (
                            <>Unlocking...</>
                        ) : (
                            <><Save size={16} /> {mode === 'keystore' ? 'Unlock & Save' : 'Save Changes'}</>
                        )}
                    </button>
                </div>
            </div>
        </div>
    )
}