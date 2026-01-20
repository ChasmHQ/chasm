import { useState, useEffect } from 'react'
import { TransactionViewer } from './TransactionViewer'
import { clsx } from 'clsx'
import type { PublicClient, Block, Transaction } from 'viem'
import { formatEther, isAddress, isHex } from 'viem'
import { Search, Box, ArrowRightLeft, FileText, Hash, Clock, Layers, ArrowLeft } from 'lucide-react'

export interface ExplorerProps {
    publicClient: PublicClient;
    rpcUrl: string;
    txModes: Record<string, 'live' | 'local'>;
    onRemoveRecentTx: (hash: string) => void;
    blockModes: Record<string, 'live' | 'local'>;
    onRemoveRecentBlock: (hash: string) => void;
    onLog: (msg: string) => void;
    recentBlocks: Block[];
    recentTransactions: Transaction[];
}

type SearchResult = 
    | { type: 'block', data: Block }
    | { type: 'tx', data: Transaction }
    | { type: 'address', address: string, balance: string, code: string, nonce: number }
    | null



export function Explorer({ publicClient, rpcUrl, txModes, onRemoveRecentTx, blockModes, onRemoveRecentBlock, onLog, recentBlocks, recentTransactions }: ExplorerProps) {
    const [query, setQuery] = useState("")
    const [result, setResult] = useState<SearchResult>(null)
    const [loading, setLoading] = useState(false)
    const [addressTxs, setAddressTxs] = useState<Transaction[]>([])
    const [loadingAddressTxs, setLoadingAddressTxs] = useState(false)
    const [notice, setNotice] = useState<string | null>(null)

    useEffect(() => {
        if (!notice) return
        const timer = setTimeout(() => setNotice(null), 3500)
        return () => clearTimeout(timer)
    }, [notice])
    
    // Trace state
    const [traceData, setTraceData] = useState<string | null>(null)
    const [loadingTrace, setLoadingTrace] = useState(false)

    const fetchTrace = async (txHash: string) => {
        setLoadingTrace(true)
        try {
            const res = await fetch(`http://localhost:3000/trace/${txHash}?rpc_url=${encodeURIComponent(rpcUrl)}`)
            const data = await res.json()
            if (data.error) setTraceData(`Error: ${data.error}`)
            else setTraceData(data.stdout || data.stderr || "No trace output.")
        } catch (e: any) {
            setTraceData(`Failed to fetch trace: ${e.message}`)
        } finally {
            setLoadingTrace(false)
        }
    }
    
    // Use recentBlocks from props instead of local state polling

    const handleNavigate = (_type: 'tx' | 'block' | 'address', value: string) => {
        setQuery(value)
        performSearch(value)
    }

    const fetchAddressTransactions = async (address: string) => {
        setLoadingAddressTxs(true)
        try {
            const currentBlock = await publicClient.getBlockNumber()
            const txs: Transaction[] = []
            
            // Scan last 1000 blocks (or less if chain is shorter)
            const startBlock = currentBlock - 1000n > 0n ? currentBlock - 1000n : 0n
            
            const promises = []
            for (let i = currentBlock; i >= startBlock; i--) {
                promises.push(publicClient.getBlock({ blockNumber: i, includeTransactions: true }))
                if (promises.length >= 20) { // Batch size 20
                    const blocks = await Promise.all(promises)
                    blocks.forEach(block => {
                        const blockTxs = block.transactions as unknown as Transaction[]
                        blockTxs.forEach(tx => {
                            if (tx.from.toLowerCase() === address.toLowerCase() || tx.to?.toLowerCase() === address.toLowerCase()) {
                                txs.push(tx)
                            }
                        })
                    })
                    promises.length = 0
                }
            }
            // Flush remaining
            if (promises.length > 0) {
                const blocks = await Promise.all(promises)
                blocks.forEach(block => {
                    const blockTxs = block.transactions as unknown as Transaction[]
                    blockTxs.forEach(tx => {
                        if (tx.from.toLowerCase() === address.toLowerCase() || tx.to?.toLowerCase() === address.toLowerCase()) {
                            txs.push(tx)
                        }
                    })
                })
            }

            setAddressTxs(txs)
        } catch (e) {
            console.error("Failed to fetch address transactions", e)
        } finally {
            setLoadingAddressTxs(false)
        }
    }

    const performSearch = async (searchQuery: string) => {
        if (!searchQuery) return
        setLoading(true)
        setResult(null)
        setAddressTxs([]) // Clear previous address txs

        try {
            // 1. Try as Address
            if (isAddress(searchQuery)) {
                const balance = await publicClient.getBalance({ address: searchQuery })
                const code = await publicClient.getBytecode({ address: searchQuery })
                const nonce = await publicClient.getTransactionCount({ address: searchQuery })
                
                setResult({ 
                    type: 'address', 
                    address: searchQuery, 
                    balance: formatEther(balance), 
                    code: code || '0x',
                    nonce 
                })

                // Trigger background fetch for all transactions
                fetchAddressTransactions(searchQuery)
                return
            }

            // 2. Try as Hash (Tx or Block)
            if (isHex(searchQuery) && searchQuery.length === 66) {
                // Try Tx
                try {
                    const tx = await publicClient.getTransaction({ hash: searchQuery })
                    // Try get Receipt too for status/gasUsed
                    let receipt = {}
                    try {
                        receipt = await publicClient.getTransactionReceipt({ hash: searchQuery })
                    } catch {}
                    
                    setResult({ type: 'tx', data: { ...tx, ...receipt } })
                    return
                } catch {
                    if (txModes[searchQuery] === 'local') {
                        onRemoveRecentTx(searchQuery)
                        setNotice("Local transaction no longer exists after revert/switch/stop. Removed from list.")
                    }
                }
                
                // Try Block Hash
                try {
                    const block = await publicClient.getBlock({ blockHash: searchQuery, includeTransactions: true })
                    setResult({ type: 'block', data: block })
                    return
                } catch {
                    if (blockModes[searchQuery] === 'local') {
                        onRemoveRecentBlock(searchQuery)
                        setNotice("Local block no longer exists after revert/switch/stop. Removed from list.")
                    }
                }
            }

            // 3. Try as Block Number
            if (/^\d+$/.test(searchQuery)) {
                try {
                    const block = await publicClient.getBlock({ blockNumber: BigInt(searchQuery), includeTransactions: true })
                    setResult({ type: 'block', data: block })
                    return
                } catch {
                    const matching = recentBlocks.find(b => b.number?.toString() === searchQuery)
                    if (matching?.hash && blockModes[matching.hash] === 'local') {
                        onRemoveRecentBlock(matching.hash)
                        setNotice("Local block no longer exists after revert/switch/stop. Removed from list.")
                    }
                }
            }
            
            onLog("Search not found")
        } catch (e: any) {
            onLog(`Search error: ${e.message}`)
        } finally {
            setLoading(false)
        }
    }

    const handleSearch = (e?: React.FormEvent) => {
        e?.preventDefault()
        performSearch(query)
    }


    return (
        <div className="flex flex-col h-full bg-slate-950 text-slate-200 p-6 overflow-y-auto">
            {notice && (
                <div className="fixed bottom-4 right-4 z-50 bg-slate-900 border border-slate-800 rounded-lg px-4 py-3 text-sm text-slate-200 shadow-lg">
                    {notice}
                </div>
            )}
            
            {/* Search Bar */}
            <div className="max-w-2xl mx-auto w-full mb-10">
                <h1 className="text-2xl font-bold mb-6 text-center tracking-tight flex items-center justify-center gap-3">
                    <Box className="text-indigo-500" /> Blockchain Explorer
                </h1>
                <form onSubmit={handleSearch} className="relative">
                    <input 
                        className="w-full bg-slate-900 border border-slate-700 rounded-lg py-3 pl-12 pr-4 text-sm focus:border-indigo-500 outline-none shadow-lg shadow-black/20"
                        placeholder="Search by Address / Tx Hash / Block..."
                        value={query}
                        onChange={e => setQuery(e.target.value)}
                    />
                    <Search className="absolute left-4 top-1/2 -translate-y-1/2 text-slate-500" size={18}/>
                </form>
            </div>

            {/* Results */}
            {loading && <div className="text-center text-slate-500">Searching...</div>}

            {!loading && result && (
                <div className="max-w-4xl mx-auto w-full space-y-6 animate-in fade-in slide-in-from-bottom-4 duration-500">
                    
                    <button 
                        onClick={() => { setResult(null); setQuery(""); }}
                        className="text-slate-500 hover:text-slate-200 flex items-center gap-2 text-sm font-medium transition-colors mb-4"
                    >
                        <ArrowLeft size={16} /> Back to Dashboard
                    </button>

                    {result.type === 'address' && (
                        <div className="bg-slate-900 border border-slate-800 rounded-lg p-6">
                            <div className="flex items-center gap-3 mb-4 border-b border-slate-800 pb-4">
                                <div className="w-10 h-10 rounded-full bg-indigo-500/20 flex items-center justify-center text-indigo-400">
                                    {result.code !== '0x' ? <FileText size={20}/> : <Hash size={20}/>}
                                </div>
                                <div>
                                    <h2 className="font-bold text-lg">{result.code !== '0x' ? "Contract" : "Address"}</h2>
                                    <p className="text-xs text-slate-500 font-mono">{result.address}</p>
                                </div>
                            </div>
                            <div className="grid grid-cols-2 gap-4 mb-6">
                                <div>
                                    <div className="text-xs text-slate-500 uppercase font-bold">Balance</div>
                                    <div className="text-xl">{result.balance} ETH</div>
                                </div>
                                <div>
                                    <div className="text-xs text-slate-500 uppercase font-bold">Nonce</div>
                                    <div className="text-xl">{result.nonce}</div>
                                </div>
                            </div>

                            {/* Correlation Txs */}
                            <div>
                                <h3 className="text-sm font-bold text-slate-500 uppercase tracking-wider mb-3 flex items-center gap-2">
                                    Transactions History 
                                    {loadingAddressTxs && <span className="text-xs font-normal text-indigo-400 animate-pulse">(Scanning chain...)</span>}
                                </h3>
                                <div className="grid gap-2">
                                    {addressTxs.length > 0 ? (
                                        addressTxs.map(tx => (
                                            <div key={tx.hash} onClick={() => handleNavigate('tx', tx.hash)} className="bg-slate-950/50 border border-slate-800 p-3 rounded flex items-center justify-between cursor-pointer hover:bg-slate-800 transition-colors">
                                                <div className="flex items-center gap-3">
                                                    <div className={clsx("px-1.5 py-0.5 rounded text-[10px] font-bold uppercase", tx.from.toLowerCase() === result.address.toLowerCase() ? "bg-orange-900/30 text-orange-400" : "bg-green-900/30 text-green-400")}>
                                                        {tx.from.toLowerCase() === result.address.toLowerCase() ? 'OUT' : 'IN'}
                                                    </div>
                                                    <div className="text-xs font-mono text-slate-300">{tx.hash.slice(0, 10)}...</div>
                                                    <div className="text-xs text-slate-500">Block #{tx.blockNumber?.toString()}</div>
                                                </div>
                                                <div className="text-xs text-slate-400">{Number(formatEther(tx.value)).toFixed(4)} ETH</div>
                                            </div>
                                        ))
                                    ) : !loadingAddressTxs ? (
                                        <div className="text-slate-600 text-xs italic">No transactions found in recent history.</div>
                                    ) : null}
                                </div>
                            </div>
                        </div>
                    )}

                    {result.type === 'tx' && (
                        <div className="bg-slate-900 border border-slate-800 rounded-lg h-[600px] overflow-hidden">
                             <TransactionViewer 
                                data={result.data}
                                onTrace={fetchTrace}
                                isLoadingTrace={loadingTrace}
                                traceData={traceData}
                                onNavigate={handleNavigate}
                             />
                        </div>
                    )}
                    
                    {result.type === 'block' && (
                        <div className="bg-slate-900 border border-slate-800 rounded-lg p-6 space-y-4">
                             <div className="flex items-center gap-2 mb-4 border-b border-slate-800 pb-4">
                                <Layers className="text-blue-500" />
                                <h2 className="font-bold">Block #{result.data.number?.toString()}</h2>
                             </div>
                             <div className="space-y-2 font-mono text-sm">
                                <div className="flex gap-4"><span className="text-slate-500 w-24">Hash:</span> <span>{result.data.hash}</span></div>
                                <div className="flex gap-4"><span className="text-slate-500 w-24">Timestamp:</span> <span>{new Date(Number(result.data.timestamp) * 1000).toLocaleString()}</span></div>
                                <div className="flex gap-4"><span className="text-slate-500 w-24">Tx Count:</span> <span>{result.data.transactions.length}</span></div>
                                <div className="flex gap-4"><span className="text-slate-500 w-24">Gas Used:</span> <span>{result.data.gasUsed.toString()}</span></div>
                             </div>

                             <div className="pt-4 border-t border-slate-800">
                                <h3 className="text-sm font-bold text-slate-500 uppercase tracking-wider mb-3">Transactions</h3>
                                <div className="grid gap-2">
                                    {(result.data.transactions as any[]).map((tx: any) => {
                                        // Handle if tx is string (hash) or object (Transaction)
                                        const hash = typeof tx === 'string' ? tx : tx.hash
                                        return (
                                            <div key={hash} onClick={() => handleNavigate('tx', hash)} className="bg-slate-950/50 border border-slate-800 p-2 rounded flex items-center justify-between cursor-pointer hover:bg-slate-800 transition-colors">
                                                <div className="text-xs font-mono text-indigo-400">{hash}</div>
                                            </div>
                                        )
                                    })}
                                    {result.data.transactions.length === 0 && <div className="text-slate-600 text-xs italic">No transactions in this block.</div>}
                                </div>
                             </div>
                        </div>
                    )}
                </div>
            )}

            {/* Dashboard (Latest Blocks & Transactions) */}
            {!result && (
                <div className="max-w-7xl mx-auto w-full mt-10 grid grid-cols-1 lg:grid-cols-2 gap-10">
                    
                    {/* Latest Blocks */}
                    <div>
                        <h3 className="text-sm font-bold text-slate-500 uppercase tracking-wider mb-4 flex items-center gap-2">
                            <Clock size={16}/> Latest Blocks
                        </h3>
                        <div className="grid gap-3">
                            {recentBlocks.map(block => (
                                <div 
                                    key={block.hash} 
                                    onClick={() => { setQuery(block.number!.toString()); performSearch(block.number!.toString()); }}
                                    className="bg-slate-900/50 border border-slate-800 p-4 rounded-lg flex items-center justify-between hover:bg-slate-800 transition-colors cursor-pointer group"
                                >
                                    <div className="flex items-center gap-4">
                                        <div className="bg-slate-950 p-2 rounded text-slate-400 font-mono text-sm border border-slate-800">
                                            #{block.number?.toString()}
                                        </div>
                                        <div>
                                            <div className="text-xs text-slate-500">Hash</div>
                                            <div className="text-xs font-mono text-slate-300 flex items-center gap-2">
                                                <span>{block.hash?.slice(0, 10)}...</span>
                                                {block.hash && blockModes[block.hash] && (
                                                    <span className={clsx(
                                                        "text-[9px] uppercase font-bold px-1.5 py-0.5 rounded",
                                                        blockModes[block.hash] === 'local'
                                                            ? "bg-indigo-600/20 text-indigo-300"
                                                            : "bg-slate-700/60 text-slate-300"
                                                    )}>
                                                        {blockModes[block.hash]}
                                                    </span>
                                                )}
                                            </div>
                                        </div>
                                    </div>
                                    <div className="text-right">
                                         <div className="text-xs text-slate-500">{block.transactions.length} Txs</div>
                                         <div className="text-xs text-slate-400">{Number(BigInt(Date.now()) / 1000n - block.timestamp)}s ago</div>
                                    </div>
                                </div>
                            ))}
                            {recentBlocks.length === 0 && <div className="text-slate-600 text-sm italic">No blocks yet...</div>}
                        </div>
                    </div>

                    {/* Latest Transactions */}
                    <div>
                         <h3 className="text-sm font-bold text-slate-500 uppercase tracking-wider mb-4 flex items-center gap-2">
                            <ArrowRightLeft size={16}/> Latest Transactions
                        </h3>
                        <div className="grid gap-3">
                            {recentTransactions.map(tx => (
                                <div 
                                    key={tx.hash} 
                                    onClick={() => { setQuery(tx.hash); performSearch(tx.hash); }}
                                    className="bg-slate-900/50 border border-slate-800 p-4 rounded-lg flex items-center justify-between hover:bg-slate-800 transition-colors cursor-pointer group"
                                >
                                     <div className="flex items-center gap-4">
                                        <div className="w-10 h-10 rounded-full bg-slate-950 border border-slate-800 flex items-center justify-center text-slate-500 font-mono text-xs">
                                            Tx
                                        </div>
                                        <div className="min-w-0">
                                            <div className="text-xs text-indigo-400 font-mono mb-1 flex items-center gap-2">
                                                <span>{tx.hash.slice(0, 18)}...</span>
                                                {txModes[tx.hash] && (
                                                    <span className={clsx(
                                                        "text-[9px] uppercase font-bold px-1.5 py-0.5 rounded",
                                                        txModes[tx.hash] === 'local'
                                                            ? "bg-indigo-600/20 text-indigo-300"
                                                            : "bg-slate-700/60 text-slate-300"
                                                    )}>
                                                        {txModes[tx.hash]}
                                                    </span>
                                                )}
                                            </div>
                                            <div className="text-[10px] text-slate-500 flex items-center gap-1">
                                                From <span className="text-slate-400 font-mono">{tx.from.slice(0,6)}...</span>
                                                to <span className="text-slate-400 font-mono">{tx.to ? tx.to.slice(0,6) + '...' : 'Contract Creation'}</span>
                                            </div>
                                        </div>
                                    </div>
                                    <div className="text-right shrink-0">
                                         <div className="text-xs text-slate-300 font-bold">{Number(formatEther(tx.value)).toFixed(4)} ETH</div>
                                    </div>
                                </div>
                            ))}
                            {recentTransactions.length === 0 && <div className="text-slate-600 text-sm italic">No transactions yet...</div>}
                        </div>
                    </div>
                </div>
            )}
        </div>
    )
}
