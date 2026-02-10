import { useState } from 'react'
import { ArrowRightLeft, Calculator, Hash, Binary, FunctionSquare } from 'lucide-react'
import { formatEther, parseEther, formatUnits, parseUnits, toHex, fromHex, keccak256 } from 'viem'
import { clsx } from 'clsx'

export function ConverterView() {
    const [activeTab, setActiveTab] = useState<'hex' | 'unit' | 'hash' | 'selector'>('hex')

    return (
        <div className="flex flex-col h-full bg-slate-950 text-slate-200">
            {/* Header / Tabs */}
            <div className="flex items-center gap-1 p-2 border-b border-slate-800 bg-slate-900/50 overflow-x-auto no-scrollbar">
                <TabButton 
                    active={activeTab === 'hex'} 
                    onClick={() => setActiveTab('hex')} 
                    icon={<Binary size={14} />} 
                    label="Hex Tools" 
                />
                <TabButton 
                    active={activeTab === 'unit'} 
                    onClick={() => setActiveTab('unit')} 
                    icon={<Calculator size={14} />} 
                    label="Unit Converter" 
                />
                <TabButton 
                    active={activeTab === 'hash'} 
                    onClick={() => setActiveTab('hash')} 
                    icon={<Hash size={14} />} 
                    label="Keccak256" 
                />
                <TabButton 
                    active={activeTab === 'selector'} 
                    onClick={() => setActiveTab('selector')} 
                    icon={<FunctionSquare size={14} />} 
                    label="Fn Selector" 
                />
            </div>

            {/* Content Area */}
            <div className="flex-1 overflow-auto p-6">
                <div className="max-w-2xl mx-auto space-y-8">
                    {activeTab === 'hex' && <HexConverter />}
                    {activeTab === 'unit' && <UnitConverter />}
                    {activeTab === 'hash' && <HashGenerator />}
                    {activeTab === 'selector' && <SelectorGenerator />}
                </div>
            </div>
        </div>
    )
}

function TabButton({ active, onClick, icon, label }: { active: boolean, onClick: () => void, icon: React.ReactNode, label: string }) {
    return (
        <button
            onClick={onClick}
            className={clsx(
                "flex items-center gap-2 px-4 py-2 rounded-md text-xs font-medium transition-all",
                active 
                    ? "bg-indigo-600 text-white shadow-lg shadow-indigo-900/20" 
                    : "text-slate-400 hover:text-slate-200 hover:bg-slate-800"
            )}
        >
            {icon}
            {label}
        </button>
    )
}

function HexConverter() {
    const [mode, setMode] = useState<'text-to-hex' | 'decimal-to-hex' | 'hex-to-text' | 'hex-to-decimal'>('text-to-hex')
    const [input, setInput] = useState('')
    const [output, setOutput] = useState('')

    const process = (val: string, currentMode: string) => {
        setInput(val)
        if (!val) { setOutput(''); return }
        try {
            switch (currentMode) {
                case 'text-to-hex':
                    setOutput(toHex(val))
                    break
                case 'decimal-to-hex':
                    setOutput(toHex(BigInt(val)))
                    break
                case 'hex-to-text':
                    if (val.startsWith('0x')) setOutput(fromHex(val as `0x${string}`, 'string'))
                    else setOutput('Must start with 0x')
                    break
                case 'hex-to-decimal':
                    if (val.startsWith('0x')) setOutput(fromHex(val as `0x${string}`, 'bigint').toString())
                    else setOutput('Must start with 0x')
                    break
            }
        } catch { setOutput('Error') }
    }

    return (
        <div className="space-y-6">
            <div className="flex gap-2 bg-slate-900 p-1 rounded-lg border border-slate-800 overflow-x-auto">
                {['text-to-hex', 'decimal-to-hex', 'hex-to-text', 'hex-to-decimal'].map(m => (
                    <button
                        key={m}
                        onClick={() => { setMode(m as any); setInput(''); setOutput(''); }}
                        className={clsx(
                            "px-3 py-1.5 rounded text-[10px] font-medium uppercase transition-colors whitespace-nowrap",
                            mode === m ? "bg-slate-800 text-indigo-400 border border-slate-700" : "text-slate-500 hover:text-slate-300"
                        )}
                    >
                        {m.replace(/-/g, ' ')}
                    </button>
                ))}
            </div>

            <div className="space-y-2">
                <label className="text-sm font-bold text-slate-500 uppercase">Input</label>
                <textarea 
                    value={input}
                    onChange={(e) => process(e.target.value, mode)}
                    className="w-full h-32 bg-slate-900 border border-slate-700 rounded-lg p-4 text-sm focus:outline-none focus:border-indigo-500 transition-colors font-mono"
                    placeholder="Enter value..."
                />
            </div>
            
            <div className="flex justify-center">
                <ArrowRightLeft className="text-slate-600" />
            </div>

            <div className="space-y-2">
                <label className="text-sm font-bold text-slate-500 uppercase">Result</label>
                <textarea 
                    readOnly
                    value={output}
                    className="w-full h-32 bg-slate-950 border border-slate-700 rounded-lg p-4 text-sm focus:outline-none transition-colors font-mono text-indigo-300"
                    placeholder="..."
                />
            </div>
        </div>
    )
}

function UnitConverter() {
    const [ether, setEther] = useState('')
    const [gwei, setGwei] = useState('')
    const [wei, setWei] = useState('')

    const updateFromEther = (val: string) => {
        setEther(val)
        if (!val) { setGwei(''); setWei(''); return }
        try {
            const w = parseEther(val)
            setWei(w.toString())
            setGwei(formatUnits(w, 9))
        } catch {}
    }

    const updateFromGwei = (val: string) => {
        setGwei(val)
        if (!val) { setEther(''); setWei(''); return }
        try {
            const w = parseUnits(val, 9)
            setWei(w.toString())
            setEther(formatEther(w))
        } catch {}
    }

    const updateFromWei = (val: string) => {
        setWei(val)
        if (!val) { setEther(''); setGwei(''); return }
        try {
            const w = BigInt(val)
            setEther(formatEther(w))
            setGwei(formatUnits(w, 9))
        } catch {}
    }
    
    return (
        <div className="space-y-4">
            <div className="grid grid-cols-1 gap-4">
                <div className="space-y-2">
                    <label className="text-sm font-bold text-slate-500">Ether (10^18)</label>
                    <input 
                        type="number" 
                        value={ether}
                        onChange={(e) => updateFromEther(e.target.value)}
                        className="w-full bg-slate-900 border border-slate-700 rounded p-3 text-lg font-mono focus:outline-none focus:border-indigo-500"
                        placeholder="1.0"
                    />
                </div>
                <div className="space-y-2">
                    <label className="text-sm font-bold text-slate-500">Gwei (10^9)</label>
                    <input 
                        type="number"
                        value={gwei}
                        onChange={(e) => updateFromGwei(e.target.value)}
                        className="w-full bg-slate-900 border border-slate-700 rounded p-3 text-lg font-mono focus:outline-none focus:border-indigo-500"
                        placeholder="1000000000"
                    />
                </div>
                <div className="space-y-2">
                    <label className="text-sm font-bold text-slate-500">Wei (10^0)</label>
                    <input 
                        type="number"
                        value={wei}
                        onChange={(e) => updateFromWei(e.target.value)}
                        className="w-full bg-slate-900 border border-slate-700 rounded p-3 text-lg font-mono focus:outline-none focus:border-indigo-500"
                        placeholder="1000000000000000000"
                    />
                </div>
            </div>
        </div>
    )
}

function HashGenerator() {
    const [input, setInput] = useState('')
    const [hash, setHash] = useState('')

    const handleChange = (val: string) => {
        setInput(val)
        try {
            // keccak256 expects bytes or hex. We convert string to bytes (hex) first.
            const hex = toHex(val)
            setHash(keccak256(hex))
        } catch { setHash('') }
    }

    return (
        <div className="space-y-4">
            <div className="space-y-2">
                <label className="text-sm font-bold text-slate-500">Input</label>
                <input 
                    value={input}
                    onChange={(e) => handleChange(e.target.value)}
                    className="w-full bg-slate-900 border border-slate-700 rounded p-3 font-mono focus:outline-none focus:border-indigo-500"
                    placeholder="Input string..."
                />
            </div>
            <div className="space-y-2">
                <label className="text-sm font-bold text-slate-500">Keccak256 Hash</label>
                <div className="p-4 bg-slate-950 border border-slate-800 rounded font-mono text-indigo-300 break-all select-all">
                    {hash || '...'}
                </div>
            </div>
        </div>
    )
}

function SelectorGenerator() {
    const [input, setInput] = useState('')
    const [selector, setSelector] = useState('')

    const handleChange = (val: string) => {
        setInput(val)
        try {
            // Function selector is first 4 bytes of keccak256(signature)
            // Example: transfer(address,uint256)
            if (!val) { setSelector(''); return }
            const hex = toHex(val)
            const hash = keccak256(hex)
            setSelector(hash.slice(0, 10)) // 0x + 8 chars
        } catch { setSelector('Error') }
    }

    return (
        <div className="space-y-4">
            <div className="p-4 bg-indigo-900/20 border border-indigo-700/50 rounded text-indigo-200 text-sm">
                Enter function signature (e.g. <code>transfer(address,uint256)</code>) to get the 4-byte selector.
            </div>
            <div className="space-y-2">
                <label className="text-sm font-bold text-slate-500">Function Signature</label>
                <input 
                    value={input}
                    onChange={(e) => handleChange(e.target.value)}
                    className="w-full bg-slate-900 border border-slate-700 rounded p-3 font-mono focus:outline-none focus:border-indigo-500"
                    placeholder="myFunction(uint256,bool)"
                />
            </div>
            <div className="space-y-2">
                <label className="text-sm font-bold text-slate-500">4-Byte Selector</label>
                <div className="p-4 bg-slate-950 border border-slate-800 rounded font-mono text-xl text-indigo-300 select-all">
                    {selector || '0x...'}
                </div>
            </div>
        </div>
    )
}