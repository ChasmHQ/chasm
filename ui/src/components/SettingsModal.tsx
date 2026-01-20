import { useState, useEffect } from 'react'
import { X, Save, Eye, EyeOff } from 'lucide-react'

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

    // Reset local state when modal opens
    useEffect(() => {
        if (isOpen) {
            setRpcUrl(currentRpcUrl)
            setPrivateKey(currentPrivateKey)
        }
    }, [isOpen, currentRpcUrl, currentPrivateKey])

    if (!isOpen) return null

    const handleSave = () => {
        onSave(rpcUrl, privateKey)
        onClose()
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
                        <p className="text-xs text-slate-600">Default: http://127.0.0.1:8545 (Anvil)</p>
                    </div>

                    {/* Private Key */}
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
                        <p className="text-xs text-slate-600">Used for signing transactions. Default is Anvil Account #0.</p>
                    </div>
                </div>

                <div className="px-4 py-3 border-t border-slate-800 bg-slate-950 flex justify-end gap-3">
                    <button 
                        onClick={onClose}
                        className="px-4 py-2 text-sm font-medium text-slate-400 hover:text-slate-200 hover:bg-slate-800 rounded transition-colors"
                    >
                        Cancel
                    </button>
                    <button 
                        onClick={handleSave}
                        className="px-4 py-2 text-sm font-medium bg-indigo-600 hover:bg-indigo-500 text-white rounded flex items-center gap-2 transition-colors"
                    >
                        <Save size={16} /> Save Changes
                    </button>
                </div>
            </div>
        </div>
    )
}
