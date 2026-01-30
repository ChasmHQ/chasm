import { useEffect, useState, useRef, useCallback, useMemo } from 'react'
import { Hammer, Zap, Plus, X, Box, ChevronRight, ChevronDown, Trash2, Globe } from 'lucide-react'
import { clsx } from 'clsx'
import { createWalletClient, createPublicClient, createTestClient, http, defineChain, type Address, type Block, type Transaction, type PublicClient, type WalletClient, type Hex, publicActions, walletActions } from 'viem'
import { privateKeyToAccount } from 'viem/accounts'

import { RequestTab } from './components/RequestTab'
import { BottomPanel } from './components/BottomPanel'
import { UserProfile } from './components/UserProfile'
import { ContractDetailsTab } from './components/ContractDetailsTab'
import { Explorer } from './components/Explorer'

// Default Constants
const DEFAULT_RPC = "http://127.0.0.1:8545"
const DEFAULT_PRIV_KEY = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"

// Anvil Chain Definition (Base)
const anvilChain = defineChain({
  id: 31337,
  name: 'Localhost',
  network: 'localhost',
  nativeCurrency: {
    decimals: 18,
    name: 'Ether',
    symbol: 'ETH',
  },
  rpcUrls: {
    default: { http: [DEFAULT_RPC] },
    public: { http: [DEFAULT_RPC] },
  },
  testnet: true,
})

interface ContractArtifact {
  name: string;
  artifact: {
      abi: any[];
      bytecode: {
          object: string;
      }
  }
}

// New Interface for Deployed Instances
interface DeployedInstance {
    id: string; // Unique ID (e.g. timestamp)
    name: string;
    address: Address;
    artifact: ContractArtifact['artifact'];
    mode?: 'live' | 'local';
}

interface StoredInstance {
    id: string;
    name: string;
    address: Address;
    mode?: 'live' | 'local';
}

interface TabData {
    id: string;
    type: 'deploy' | 'function' | 'details';
    contractName: string;
    label: string;
    abiItem?: any; 
    bytecode?: string;
    instanceId?: string; // Links tab to specific deployed instance
}


interface SnapshotEntry {
  id: string;
  snapshotId: string;
  createdAt: number;
  mode: 'local';
  method: string;
  from?: string;
  to?: string;
  value?: string;
  txHash?: string;
  blockNumber?: number;
  status: 'pending' | 'confirmed' | 'error';
}

interface LogEntry {
  message: string;
  timestamp: string;
}

function App() {
  const [contracts, setContracts] = useState<ContractArtifact[]>([])
  const contractsRef = useRef<Map<string, string>>(new Map())
  const [logs, setLogs] = useState<LogEntry[]>([])
  
  // Settings State
  const [rpcUrl, setRpcUrl] = useState(DEFAULT_RPC)
  const [privateKey, setPrivateKey] = useState(DEFAULT_PRIV_KEY)
  const [chainId, setChainId] = useState<number>(31337)
  const [isRpcConnected, setIsRpcConnected] = useState(false)
  const [globalMode, setGlobalMode] = useState<'live' | 'local'>('live')
  const [localForkBlock, setLocalForkBlock] = useState("")
  const [localForkStatus, setLocalForkStatus] = useState<{ running: boolean; port?: number | null } | null>(null)

  // Tab State
  const [tabs, setTabs] = useState<TabData[]>([])
  const [activeTabId, setActiveTabId] = useState<string | null>(null)
  const tabsRef = useRef<TabData[]>([])

  // Deployed Instances State
  const [deployedInstances, setDeployedInstances] = useState<DeployedInstance[]>([])
  const [storedInstances, setStoredInstances] = useState<StoredInstance[]>([])
  const deployedInstancesRef = useRef<DeployedInstance[]>([])
  const hasLoadedStoredInstances = useRef(false)
  const isHydratingStoredInstances = useRef(false)

  // Auto Deploy State

  // UI Toast State
  const [updateToast, setUpdateToast] = useState<{ message: string } | null>(null)
  
  // Sidebar Sections State
    const [isWorkspaceOpen, setIsWorkspaceOpen] = useState(true)
    const [isDeployedOpen, setIsDeployedOpen] = useState(true)
    const [isLocalDeployedOpen, setIsLocalDeployedOpen] = useState(true)

  // View State
  const [activeView, setActiveView] = useState<'contracts' | 'explorer'>('contracts')


  // Explorer Data State
  const [recentBlocks, setRecentBlocks] = useState<Block[]>([])
  const [recentTransactions, setRecentTransactions] = useState<Transaction[]>([])
  const [txModes, setTxModes] = useState<Record<string, 'live' | 'local'>>({})
  const [blockModes, setBlockModes] = useState<Record<string, 'live' | 'local'>>({})

  const hasLoadedSettings = useRef(false)
  const [snapshots, setSnapshots] = useState<SnapshotEntry[]>([])
  const [activeSnapshotId, setActiveSnapshotId] = useState<string | null>(null)
  const [snapshotToast, setSnapshotToast] = useState<{ message: string; status: 'success' | 'error' } | null>(null)

  // Viem Clients (Memoized)
  const clients = useMemo(() => {
    try {
        const account = privateKeyToAccount(privateKey as `0x${string}`)
        const dynamicChain = defineChain({
            id: chainId,
            name: 'Network',
            network: 'network',
            nativeCurrency: {
              decimals: 18,
              name: 'Ether',
              symbol: 'ETH',
            },
            rpcUrls: {
                default: { http: [rpcUrl] },
                public: { http: [rpcUrl] },
            }
        })
        
        const transport = http(rpcUrl)

        const testClient = createTestClient({
            chain: dynamicChain,
            mode: 'anvil',
            transport
        })
        .extend(publicActions)
        .extend(walletActions)
        
        return {
            account,
            walletClient: createWalletClient({
                account,
                chain: dynamicChain,
                transport
            }),
            publicClient: createPublicClient({
                chain: dynamicChain,
                transport
            }),
            testClient
        }
    } catch (e) {
        console.error("Failed to create clients", e)
        return null
    }
  }, [rpcUrl, privateKey, chainId])

  const ANVIL_DEFAULT_KEY = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

  useEffect(() => {
    const raw = localStorage.getItem("chainsmith.settings")
    if (!raw) {
      hasLoadedSettings.current = true
      return
    }
    try {
      const parsed = JSON.parse(raw) as {
        rpcUrl?: string
        privateKey?: string
        globalMode?: 'live' | 'local'
        localForkBlock?: string
      }
      if (parsed?.rpcUrl) setRpcUrl(parsed.rpcUrl)
      // SECURITY: Do NOT load private key from local storage. Always default to Anvil #0 or empty.
      // We explicitly ignore parsed.privateKey to prevent persistence of sensitive keys.
      setPrivateKey(ANVIL_DEFAULT_KEY)
      
      if (parsed?.globalMode) setGlobalMode(parsed.globalMode)
      if (typeof parsed?.localForkBlock === "string") setLocalForkBlock(parsed.localForkBlock)
    } catch (e) {
      console.error("Failed to load settings", e)
    }
    hasLoadedSettings.current = true
  }, [])

  useEffect(() => {
    if (!hasLoadedSettings.current) return
    try {
      // SECURITY: Never save the private key to local storage.
      localStorage.setItem(
        "chainsmith.settings",
        JSON.stringify({ rpcUrl, globalMode, localForkBlock }) // privateKey excluded
      )
    } catch (e) {
      console.error("Failed to persist settings", e)
    }
  }, [rpcUrl, globalMode, localForkBlock])

  useEffect(() => {
    const raw = localStorage.getItem("chainsmith.deployedInstances")
    if (!raw) {
      hasLoadedStoredInstances.current = true
      return
    }
    try {
      const parsed = JSON.parse(raw) as StoredInstance[]
      if (Array.isArray(parsed)) {
        isHydratingStoredInstances.current = true
        setStoredInstances(parsed)
      }
    } catch (e) {
      console.error("Failed to load deployed instances", e)
    }
    hasLoadedStoredInstances.current = true
  }, [])

  useEffect(() => {
    if (!hasLoadedStoredInstances.current) return
    if (isHydratingStoredInstances.current) {
      isHydratingStoredInstances.current = false
      return
    }
    try {
      localStorage.setItem("chainsmith.deployedInstances", JSON.stringify(storedInstances))
    } catch (e) {
      console.error("Failed to persist deployed instances", e)
    }
  }, [storedInstances])

  useEffect(() => {
    tabsRef.current = tabs
  }, [tabs])

  useEffect(() => {
    let mounted = true
    const fetchForkStatus = async () => {
      try {
        const res = await fetch("/fork/status")
        const data = await res.json()
        if (!mounted) return
        if (data?.running && data?.port) {
          setLocalForkStatus({ running: true, port: data.port })
        } else {
          setLocalForkStatus({ running: false })
        }
      } catch {
        if (mounted) setLocalForkStatus({ running: false })
      }
    }
    fetchForkStatus()
    const interval = setInterval(fetchForkStatus, 5000)
    return () => {
      mounted = false
      clearInterval(interval)
    }
  }, [])


  useEffect(() => {
    if (storedInstances.length === 0) {
      setDeployedInstances([])
      return
    }
      const next = storedInstances.map(inst => {
        const contract = contracts.find(c => c.name === inst.name)
        return {
          ...inst,
          mode: inst.mode ?? 'live',
          artifact: contract?.artifact || { abi: [], bytecode: { object: "0x" } }
        }
      })
    setDeployedInstances(next)
  }, [storedInstances, contracts])

  useEffect(() => {
    deployedInstancesRef.current = deployedInstances
  }, [deployedInstances])

  // Resize State
  const [bottomHeight, setBottomHeight] = useState(200) // px
  const isResizingRef = useRef(false)
  const containerRef = useRef<HTMLDivElement>(null)

  // RPC Health Check
  useEffect(() => {
      let mounted = true
      const checkConnection = async () => {
          if (!clients) {
              if(mounted) setIsRpcConnected(false)
              return
          }
          try {
              const remoteChainId = await clients.publicClient.getChainId()
              if(mounted) {
                  if (remoteChainId !== chainId) {
                      setChainId(remoteChainId)
                  }
                  setIsRpcConnected(true)
              }
          } catch (e) {
              if(mounted) setIsRpcConnected(false)
          }
      }

      checkConnection()
      const interval = setInterval(checkConnection, 5000)
      return () => {
          mounted = false
          clearInterval(interval)
      }
  }, [clients])

  useEffect(() => {
    const ws = new WebSocket("ws://localhost:3000/ws")

    ws.onopen = () => {
      setLogs(p => [...p, { message: "Connected to ChainSmith Engine", timestamp: new Date().toLocaleTimeString() }])
    }

    ws.onmessage = (event) => {
      try {
        const data = JSON.parse(event.data)
        if (data.type === 'compile_success') {
          setLogs(p => [...p, { message: "Compilation successful!", timestamp: new Date().toLocaleTimeString() }])
          if (data.contracts && Array.isArray(data.contracts)) {
             const nextMap = new Map<string, string>()
             const changed: string[] = []
             data.contracts.forEach((c: ContractArtifact) => {
               const fingerprint = `${c.name}:${c.artifact?.bytecode?.object || ""}`
               const prev = contractsRef.current.get(c.name)
               if (prev && prev !== fingerprint) {
                 changed.push(c.name)
               }
               nextMap.set(c.name, fingerprint)
             })
             contractsRef.current = nextMap
             setContracts(data.contracts)
             if (changed.length > 0) {
               const label = changed.length === 1 ? `${changed[0]}.sol` : `${changed.map(n => `${n}.sol`).join(", ")}`
               setUpdateToast({ message: `Contract ${label} updated, click to refresh.` })
             }
          }
        } else if (data.type === 'compile_error') {
            setLogs(p => [...p, { message: `Error: ${data.error}`, timestamp: new Date().toLocaleTimeString() }])
        }
      } catch (e) {
        setLogs(p => [...p, { message: String(event.data), timestamp: new Date().toLocaleTimeString() }])
      }
    }

    ws.onclose = () => {}
    return () => ws.close()
  }, [])

  const log = (msg: string) => setLogs(p => [...p, { message: msg, timestamp: new Date().toLocaleTimeString() }])
  const openDeployTab = (contract: ContractArtifact, activate = true) => {
      const id = `deploy-${contract.name}`
      const existing = tabs.find(t => t.id === id)
      if (existing) {
          if (activate) setActiveTabId(id)
          return id
      }

      const ctor = contract.artifact.abi.find((item: any) => item.type === 'constructor') || { inputs: [], type: 'constructor', stateMutability: 'nonpayable' }
      
      const newTab: TabData = {
          id,
          type: 'deploy',
          contractName: contract.name,
          label: `Deploy ${contract.name}`,
          abiItem: ctor,
          bytecode: contract.artifact.bytecode.object
      }
      setTabs(prev => [...prev, newTab])
      if (activate) setActiveTabId(id)
      return id
  }

  // Open Function for a specific INSTANCE
  const openInstanceFunctionTab = (instance: DeployedInstance, func: any) => {
      if (instance.mode === 'local' && globalMode !== 'local') {
          setGlobalMode('local')
      }
      const id = `func-${instance.id}-${func.name}`
      const existing = tabs.find(t => t.id === id)
      if (existing) {
          setActiveTabId(id)
          return
      }

      const newTab: TabData = {
          id,
          type: 'function',
          contractName: instance.name,
          label: `${func.name}`,
          abiItem: func,
          instanceId: instance.id
      }
      setTabs(prev => [...prev, newTab])
      setActiveTabId(id)
  }

  // Open Details for a specific INSTANCE
  const openInstanceDetailsTab = (instance: DeployedInstance) => {
      if (instance.mode === 'local' && globalMode !== 'local') {
          setGlobalMode('local')
      }
      const id = `details-${instance.id}`
      const existing = tabs.find(t => t.id === id)
      if (existing) {
          setActiveTabId(id)
          return
      }

      const newTab: TabData = {
          id,
          type: 'details',
          contractName: instance.name,
          label: `${instance.name} (${instance.address.slice(0,6)}...)`,
          bytecode: instance.artifact.bytecode.object,
          instanceId: instance.id
      }
      setTabs(prev => [...prev, newTab])
      setActiveTabId(id)
  }

  // Handle Deploy Success -> Create Instance
  const handleDeploySuccess = (
      contractName: string,
      address: Address,
  ) => {
      const contract = contracts.find(c => c.name === contractName)
      if (contract) {
          const newInstance: DeployedInstance = {
              id: `${contractName}-${address}-${Date.now()}`,
              name: contractName,
              address,
              artifact: contract.artifact,
              mode: globalMode
          }

          setDeployedInstances(prev => [
              newInstance,
              ...prev
          ]) // Newest first
          setStoredInstances(prev => [
              {
                  id: newInstance.id,
                  name: newInstance.name,
                  address: newInstance.address,
                  mode: newInstance.mode
              },
              ...prev
          ])
          setIsDeployedOpen(true)
          
          // Optionally auto-open details tab for new instance
          openInstanceDetailsTab(newInstance)
      }
  }

  const removeInstance = (id: string, e: React.MouseEvent) => {
      e.stopPropagation()
      setDeployedInstances(prev => prev.filter(i => i.id !== id))
      setStoredInstances(prev => prev.filter(i => i.id !== id))
      // Close related tabs? Maybe keep them open but invalid? 
      // Ideally close them.
      setTabs(prev => prev.filter(t => t.instanceId !== id))
  }

  const closeTab = (e: React.MouseEvent, id: string) => {
      e.stopPropagation()
      setTabs(prev => {
          const newTabs = prev.filter(t => t.id !== id)
          if (activeTabId === id && newTabs.length > 0) {
              setActiveTabId(newTabs[newTabs.length - 1].id)
          } else if (newTabs.length === 0) {
              setActiveTabId(null)
          }
          return newTabs
      })
  }

  const handleRefreshContracts = () => {
    setDeployedInstances(prev => prev.map(inst => {
      const updated = contracts.find(c => c.name === inst.name)
      return updated ? { ...inst, artifact: updated.artifact } : inst
    }))
    setUpdateToast(null)
  }

  const buildForkClients = useCallback((forkRpc: string) => {
    const account = privateKeyToAccount(privateKey as `0x${string}`)
    const chainConfig = {
      ...anvilChain,
      rpcUrls: {
        default: { http: [forkRpc] },
        public: { http: [forkRpc] },
      }
    }
    const transport = http(forkRpc)
    const publicClient = createPublicClient({ chain: chainConfig, transport }) as PublicClient
    const walletClient = createWalletClient({ account, chain: chainConfig, transport }) as WalletClient
    const testClient = createTestClient({ chain: chainConfig, mode: 'anvil', transport })
      .extend(publicActions)
      .extend(walletActions)
    return { publicClient, walletClient, testClient, rpcUrl: forkRpc }
  }, [privateKey])

  const localClients = useMemo(() => {
    if (!localForkStatus?.running || !localForkStatus.port) return null
    const forkRpc = `http://127.0.0.1:${localForkStatus.port}`
    try {
      return buildForkClients(forkRpc)
    } catch (e) {
      console.error("Failed to create local fork clients", e)
      return null
    }
  }, [localForkStatus?.running, localForkStatus?.port, buildForkClients])


  // Poll for blocks and transactions
  useEffect(() => {
    const explorerClient =
      globalMode === "local" ? localClients?.publicClient : clients?.publicClient
    if (!explorerClient) return

    const fetchLatest = async () => {
      try {
        const block = await explorerClient.getBlock({ includeTransactions: true })
        
        setRecentBlocks(prev => {
          if (prev.length > 0 && prev[0].number === block.number) return prev
          return [block, ...prev].slice(0, 50)
        })
        setBlockModes(prev => {
          const hash = block.hash || ''
          if (!hash) return prev
          if (prev[hash]) return prev
          return { ...prev, [hash]: globalMode }
        })

        if (block.transactions.length > 0) {
            // viem types: block.transactions is Transaction[] if includeTransactions is true
            const txs = block.transactions as unknown as Transaction[]
            setRecentTransactions(prev => {
                // Avoid duplicates (if we fetch same block twice or reorg?)
                // Just filtering by hash should be enough.
                // Newest txs are at the end of block.transactions usually? or ordered by index.
                // We want newest first in our list.
                const newTxs = txs.filter(tx => !prev.some(p => p.hash === tx.hash))
                if (newTxs.length === 0) return prev
                // Reverse to have newest (highest index) first if that's the order
                return [...newTxs.reverse(), ...prev].slice(0, 100)
            })
            setTxModes(prev => {
              const next = { ...prev }
              txs.forEach(tx => {
                if (!next[tx.hash]) {
                  next[tx.hash] = globalMode
                }
              })
              return next
            })
        }
      } catch (e) {
        console.error("Block fetch error", e)
      }
    }

    fetchLatest()
    const interval = setInterval(fetchLatest, 4000)
    return () => clearInterval(interval)
  }, [clients?.publicClient, localClients?.publicClient, globalMode])

  useEffect(() => {
    if (globalMode !== 'live') return
    setRecentTransactions(prev => prev.filter(tx => txModes[tx.hash] !== 'local'))
    setRecentBlocks(prev => prev.filter(block => (block.hash ? blockModes[block.hash] !== 'local' : true)))
    setTxModes(prev => {
      const next: Record<string, 'live' | 'local'> = {}
      Object.entries(prev).forEach(([hash, mode]) => {
        if (mode !== 'local') next[hash] = mode
      })
      return next
    })
    setBlockModes(prev => {
      const next: Record<string, 'live' | 'local'> = {}
      Object.entries(prev).forEach(([hash, mode]) => {
        if (mode !== 'local') next[hash] = mode
      })
      return next
    })
  }, [globalMode, txModes, blockModes])

  const startLocalFork = useCallback(async () => {
    const rawBlock = localForkBlock.trim()
    const blockNumber = rawBlock ? Number(rawBlock) : undefined
    if (rawBlock && Number.isNaN(blockNumber)) {
      throw new Error("Invalid fork block number")
    }
    await fetch("http://localhost:3000/fork/start", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ rpcUrl, blockNumber }),
    })
    const res = await fetch("http://localhost:3000/fork/status")
    const data = await res.json()
    if (data?.running && data?.port) {
      setLocalForkStatus({ running: true, port: data.port })
      return `http://127.0.0.1:${data.port}`
    }
    throw new Error("Failed to start local fork")
  }, [rpcUrl, localForkBlock])

  const stopLocalFork = useCallback(async () => {
    await fetch("http://localhost:3000/fork/stop", { method: "POST" })
    setLocalForkStatus({ running: false })
    setSnapshots([])
    setActiveSnapshotId(null)
  }, [])

  const ensureLocalClients = useCallback(async () => {
    if (localClients) return localClients
    const forkRpc = await startLocalFork()
    return buildForkClients(forkRpc)
  }, [localClients, startLocalFork, buildForkClients])

  useEffect(() => {
    if (globalMode !== "local") return
    if (activeView !== "explorer") return
    if (localClients) return
    void ensureLocalClients()
  }, [globalMode, activeView, localClients, ensureLocalClients])

  const addSnapshot = useCallback((entry: {
    snapshotId: string;
    method: string;
    from?: string;
    to?: string;
    value?: string;
  }) => {
    const id = `snap-${Date.now()}-${Math.random().toString(36).slice(2, 7)}`
    const next: SnapshotEntry = {
      id,
      createdAt: Date.now(),
      status: 'pending',
      mode: 'local',
      ...entry,
    }
    setSnapshots(prev => [next, ...prev])
    setActiveSnapshotId(entry.snapshotId)
    return id
  }, [])

  const updateSnapshot = useCallback((id: string, patch: Partial<SnapshotEntry>) => {
    setSnapshots(prev => prev.map(s => s.id === id ? { ...s, ...patch } : s))
  }, [])

  const revertSnapshot = useCallback(async (snapshotId: string) => {
    try {
      const clients = await ensureLocalClients()
      await clients.testClient.revert({ id: snapshotId as Hex })
      setActiveSnapshotId(snapshotId)
      setSnapshots(prev => {
        const target = prev.find(s => s.snapshotId === snapshotId)
        if (!target) return prev
        return prev.filter(s => s.createdAt <= target.createdAt)
      })
      setSnapshotToast({ message: "Reverted to snapshot", status: "success" })
      log(`Reverted to snapshot ${snapshotId}`)
    } catch (e: any) {
      setSnapshotToast({ message: `Revert failed: ${e.message || e}`, status: "error" })
    }
  }, [ensureLocalClients, log])

  useEffect(() => {
    if (!snapshotToast) return
    const timer = setTimeout(() => setSnapshotToast(null), 3500)
    return () => clearTimeout(timer)
  }, [snapshotToast])

  // Resizing Logic
  const startResizing = useCallback(() => {
      isResizingRef.current = true
      document.body.style.cursor = 'row-resize'
      document.body.style.userSelect = 'none'
  }, [])

  const stopResizing = useCallback(() => {
      isResizingRef.current = false
      document.body.style.cursor = ''
      document.body.style.userSelect = ''
  }, [])

  const handleMouseMove = useCallback((e: MouseEvent) => {
      if (!isResizingRef.current || !containerRef.current) return
      
      const containerRect = containerRef.current.getBoundingClientRect()
      const newHeight = containerRect.bottom - e.clientY
      
      if (newHeight > 20 && newHeight < containerRect.height - 50) {
          setBottomHeight(newHeight)
      }
  }, [])

  useEffect(() => {
      window.addEventListener('mousemove', handleMouseMove)
      window.addEventListener('mouseup', stopResizing)
      return () => {
          window.removeEventListener('mousemove', handleMouseMove)
          window.removeEventListener('mouseup', stopResizing)
      }
  }, [handleMouseMove, stopResizing])

  return (
    <>
    <div className="flex h-screen w-screen bg-slate-950 text-slate-200 overflow-hidden font-sans">
      
      {/* Activity Bar */}
      <div className="w-12 flex flex-col items-center py-4 bg-slate-950 border-r border-slate-800 shrink-0 gap-4 z-50">
          <div className="p-2 bg-slate-900 rounded-lg mb-2">
             <div className="w-6 h-6 bg-indigo-600 rounded-md flex items-center justify-center font-bold text-white text-[10px] shadow-lg shadow-indigo-500/20">CS</div>
          </div>
          
          <button 
            onClick={() => setActiveView('contracts')}
            className={clsx("p-2 rounded-lg transition-all relative group", activeView === 'contracts' ? "text-indigo-400 bg-slate-900" : "text-slate-500 hover:text-slate-300")}
            title="Contracts"
          >
            <Hammer size={20} strokeWidth={1.5} />
            {activeView === 'contracts' && <div className="absolute left-0 top-1/2 -translate-y-1/2 w-0.5 h-6 bg-indigo-500 rounded-r-full" />}
          </button>

          <button 
            onClick={() => setActiveView('explorer')}
            className={clsx("p-2 rounded-lg transition-all relative group", activeView === 'explorer' ? "text-indigo-400 bg-slate-900" : "text-slate-500 hover:text-slate-300")}
            title="Explorer"
          >
            <Globe size={20} strokeWidth={1.5} />
            {activeView === 'explorer' && <div className="absolute left-0 top-1/2 -translate-y-1/2 w-0.5 h-6 bg-indigo-500 rounded-r-full" />}
          </button>

          <div className="flex-1" />
          
          <UserProfile 
            address={clients?.account.address}
            rpcUrl={rpcUrl}
            privateKey={privateKey}
            isConnected={isRpcConnected}
            publicClient={clients ? clients.publicClient : null}
            onUpdateSettings={(newRpc, newKey) => {
                setRpcUrl(newRpc)
                setPrivateKey(newKey)
                log(`Updated Settings: RPC=${newRpc}`)
            }}
            compact={true}
          />
      </div>

      {/* Sidebar */}
      {activeView === 'contracts' && (
      <div className="w-64 border-r border-slate-800 bg-slate-900 flex flex-col shrink-0">
        <div className="flex-1 overflow-y-auto custom-scrollbar">
            
            {/* Workspace Section */}
            <div className="border-b border-slate-800">
                <div 
                    onClick={() => setIsWorkspaceOpen(!isWorkspaceOpen)}
                    className="flex items-center justify-between px-4 py-2 bg-slate-900 hover:bg-slate-800 cursor-pointer select-none"
                >
                    <span className="text-[10px] font-bold text-slate-500 uppercase tracking-wider">Workspace</span>
                    {isWorkspaceOpen ? <ChevronDown size={14} className="text-slate-500"/> : <ChevronRight size={14} className="text-slate-500"/>}
                </div>
                
                {isWorkspaceOpen && (
                    <div className="p-2 space-y-1">
                        {contracts.length === 0 && <div className="px-2 text-xs text-slate-600 italic">No contracts found.</div>}
                        {contracts.map(c => (
                            <div key={c.name} className="flex items-center justify-between px-2 py-1.5 hover:bg-slate-800 rounded group transition-colors">
                                <span className="flex items-center gap-2 text-sm font-medium text-slate-300">
                                    <Box size={14} className="text-slate-500"/>
                                    {c.name}
                                </span>
                                <button 
                                    onClick={(e) => { e.stopPropagation(); openDeployTab(c); }}
                                    title="Deploy"
                                    className="opacity-0 group-hover:opacity-100 p-1 hover:bg-indigo-500/20 text-indigo-400 rounded transition-all"
                                >
                                    <Plus size={14} />
                                </button>
                            </div>
                        ))}
                    </div>
                )}
            </div>

            {/* Deployed Section */}
            <div>
                <div 
                    onClick={() => setIsDeployedOpen(!isDeployedOpen)}
                    className="flex items-center justify-between px-4 py-2 bg-slate-900 hover:bg-slate-800 cursor-pointer select-none border-b border-slate-800"
                >
                    <span className="text-[10px] font-bold text-slate-500 uppercase tracking-wider">Deployed</span>
                    {isDeployedOpen ? <ChevronDown size={14} className="text-slate-500"/> : <ChevronRight size={14} className="text-slate-500"/>}
                </div>

                {isDeployedOpen && (
                    <div className="p-2 space-y-2">
                        {deployedInstances.filter(inst => inst.mode !== 'local').length === 0 && (
                            <div className="px-2 text-xs text-slate-600 italic py-2 text-center">No deployments yet.</div>
                        )}
                        {deployedInstances.filter(inst => inst.mode !== 'local').map(inst => (
                            <div key={inst.id} className="bg-slate-800/30 border border-slate-800 rounded-md overflow-hidden">
                                {/* Instance Header */}
                                <div 
                                    onClick={() => openInstanceDetailsTab(inst)}
                                    className="px-3 py-2 flex items-center justify-between cursor-pointer hover:bg-slate-800 transition-colors group"
                                >
                                    <div>
                                        <div className="flex items-center gap-1.5 font-bold text-xs text-slate-200">
                                            {inst.name}
                                            <span className="text-[10px] font-normal text-slate-500">({inst.address.slice(0,4)}...{inst.address.slice(-4)})</span>
                                        </div>
                                    </div>
                                    <button 
                                        onClick={(e) => removeInstance(inst.id, e)}
                                        className="opacity-0 group-hover:opacity-100 text-slate-500 hover:text-red-400 p-1 rounded"
                                    >
                                        <Trash2 size={12} />
                                    </button>
                                </div>
                                <div className="px-3 pb-2 space-y-0.5">
                                    {inst.artifact.abi
                                        .filter((item: any) => item.type === 'function')
                                        .map((func: any, i: number) => {
                                            const isView = func.stateMutability === 'view' || func.stateMutability === 'pure'
                                            return (
                                                <button 
                                                    key={i}
                                                    onClick={() => openInstanceFunctionTab(inst, func)}
                                                    className="w-full text-left px-2 py-1 text-[11px] text-slate-400 hover:text-slate-100 hover:bg-slate-700/50 rounded flex items-center gap-1.5 transition-colors truncate"
                                                >
                                                    <div className={clsx("w-1 h-1 rounded-full", isView ? "bg-blue-500" : "bg-orange-500")} />
                                                    {func.name}
                                                </button>
                                            )
                                        })
                                    }
                                </div>
                            </div>
                        ))}
                    </div>
                )}
            </div>

            <div>
                <div 
                    onClick={() => setIsLocalDeployedOpen(!isLocalDeployedOpen)}
                    className="flex items-center justify-between px-4 py-2 bg-slate-900 hover:bg-slate-800 cursor-pointer select-none border-b border-slate-800"
                >
                    <span className="text-[10px] font-bold text-slate-500 uppercase tracking-wider">Local Deployed</span>
                    <div className="flex items-center gap-2">
                        {isLocalDeployedOpen ? <ChevronDown size={14} className="text-slate-500"/> : <ChevronRight size={14} className="text-slate-500"/>}
                    </div>
                </div>

                {isLocalDeployedOpen && (
                    <div className="p-2 space-y-2">
                        {deployedInstances.filter(inst => inst.mode === 'local').length === 0 && (
                            <div className="px-2 text-xs text-slate-600 italic py-2 text-center">No local deployments yet.</div>
                        )}
                        {deployedInstances.filter(inst => inst.mode === 'local').map(inst => (
                            <div key={inst.id} className="bg-slate-800/30 border border-slate-800 rounded-md overflow-hidden">
                                <div 
                                    onClick={() => openInstanceDetailsTab(inst)}
                                    className={clsx(
                                        "px-3 py-2 flex items-center justify-between cursor-pointer transition-colors group",
                                        globalMode === 'local' ? "hover:bg-slate-800" : "opacity-70 hover:bg-slate-800/40"
                                    )}
                                >
                                    <div>
                                        <div className="flex items-center gap-1.5 font-bold text-xs text-slate-200">
                                            {inst.name}
                                            <span className="text-[10px] font-normal text-slate-500">({inst.address.slice(0,4)}...{inst.address.slice(-4)})</span>
                                        </div>
                                    </div>
                                    <button 
                                        onClick={(e) => removeInstance(inst.id, e)}
                                        className="opacity-0 group-hover:opacity-100 text-slate-500 hover:text-red-400 p-1 rounded"
                                    >
                                        <Trash2 size={12} />
                                    </button>
                                </div>
                                <div className="px-3 pb-2 space-y-0.5">
                                    {inst.artifact.abi
                                        .filter((item: any) => item.type === 'function')
                                        .map((func: any, i: number) => {
                                            const isView = func.stateMutability === 'view' || func.stateMutability === 'pure'
                                            return (
                                                <button 
                                                    key={i}
                                                    onClick={() => openInstanceFunctionTab(inst, func)}
                                                    className={clsx(
                                                        "w-full text-left px-2 py-1 text-[11px] flex items-center gap-1.5 transition-colors truncate",
                                                        globalMode === 'local'
                                                            ? "text-slate-400 hover:text-slate-100 hover:bg-slate-700/50"
                                                            : "text-slate-600 hover:text-slate-300 hover:bg-slate-800/40"
                                                    )}
                                                >
                                                    <div className={clsx("w-1 h-1 rounded-full", isView ? "bg-blue-500" : "bg-orange-500")} />
                                                    {func.name}
                                                </button>
                                            )
                                        })
                                    }
                                </div>
                            </div>
                        ))}
                    </div>
                )}
            </div>

        </div>
      </div>
      )}

      {/* Main Content */}
      <div ref={containerRef} className="flex-1 flex flex-col min-w-0 bg-slate-950 relative">
        {/* Global Mode Bar */}
        <div className="flex items-center justify-between px-4 py-2 border-b border-slate-800 bg-slate-900/60">
          <div className="flex items-center gap-2 text-xs font-semibold uppercase text-slate-400">
            <span>Mode</span>
            <div className="flex bg-slate-800 rounded p-0.5">
              <button
                onClick={() => setGlobalMode('live')}
                className={clsx(
                  "px-3 py-1 text-[10px] font-medium rounded-sm transition-all",
                  globalMode === 'live'
                    ? "bg-slate-600 text-slate-100 shadow-sm"
                    : "text-slate-400 hover:text-slate-300",
                )}
              >
                Live
              </button>
              <button
                onClick={() => setGlobalMode('local')}
                className={clsx(
                  "px-3 py-1 text-[10px] font-medium rounded-sm transition-all",
                  globalMode === 'local'
                    ? "bg-indigo-600 text-white shadow-sm"
                    : "text-slate-400 hover:text-slate-300",
                )}
              >
                Local
              </button>
            </div>
            <span className={clsx("text-[10px]", localForkStatus?.running ? "text-emerald-400" : "text-slate-500")}>
              {localForkStatus?.running ? "Running" : "Stopped"}
            </span>
          </div>
          <div className="flex items-center gap-3">
            {globalMode === 'local' && (
              <div className="flex items-center gap-2 text-xs text-slate-400">
                <span className="uppercase text-[10px] font-bold">Block</span>
                <input
                  className="w-28 bg-slate-900 border border-slate-800 rounded px-2 py-1 text-[10px] text-slate-200 focus:outline-none focus:border-indigo-500"
                  placeholder="Latest"
                  value={localForkBlock}
                  onChange={(e) => setLocalForkBlock(e.target.value)}
                />
              </div>
            )}
            {localForkStatus?.running && (
              <button
                onClick={() => stopLocalFork()}
                className="text-[10px] uppercase font-bold px-3 py-1 rounded border border-slate-700 text-slate-300 hover:text-white hover:bg-slate-800"
              >
                Stop Local Fork
              </button>
            )}
          </div>
        </div>
        {/* View Content */}
        <div className="flex-1 min-h-0 relative flex flex-col">
        {activeView === 'contracts' ? (
        <>
        {/* Tabs Header */}
        <div className="flex bg-slate-900 border-b border-slate-800 overflow-x-auto no-scrollbar h-[40px] shrink-0">
            {tabs.map(tab => (
                <div 
                    key={tab.id}
                    onClick={() => setActiveTabId(tab.id)}
                    className={clsx(
                        "flex items-center gap-2 px-3 py-2 text-xs font-medium border-r border-slate-800 cursor-pointer min-w-[120px] max-w-[200px] select-none group h-full",
                        activeTabId === tab.id 
                            ? "bg-slate-950 text-indigo-400 border-t-2 border-t-orange-500" 
                            : "text-slate-500 hover:text-slate-300 hover:bg-slate-800"
                    )}
                >
                    <span className={clsx(
                        "uppercase text-[9px] px-1 rounded font-bold shrink-0", 
                        tab.type === 'deploy' ? "bg-yellow-900/30 text-yellow-500" : 
                        tab.type === 'details' ? "bg-purple-900/30 text-purple-500" :
                        "bg-blue-900/30 text-blue-500"
                    )}>
                        {tab.type === 'deploy' ? 'DEP' : tab.type === 'details' ? 'DET' : 'FN'}
                    </span>
                    <span className="truncate flex-1">{tab.label}</span>
                    <button onClick={(e) => closeTab(e, tab.id)} className="opacity-0 group-hover:opacity-100 hover:bg-slate-700 rounded p-0.5 shrink-0">
                        <X size={12} />
                    </button>
                </div>
            ))}
        </div>

        {/* Workspace */}
        <div className="flex-1 min-h-0 relative flex flex-col">
            
            {/* Tab Content (Top) */}
            <div className="flex-1 min-h-0 relative flex flex-col">
                 {tabs.length === 0 && (
                    <div className="flex-1 flex flex-col items-center justify-center text-slate-700 gap-4 select-none">
                        <Zap size={64} strokeWidth={1} />
                        <p>Select a contract function to start.</p>
                    </div>
                 )}

                 {clients && tabs.map(tab => {
                     // Resolve data from either contract artifacts or deployed instance
                     // If it's a function or details, we need to know the context.
                     // Logic:
                     // 1. If instanceId is present, use that instance.
                     // 2. If not, fallback to contract artifact (for deploy tabs)
                     
                     let contractAddress: Address | undefined | null = null
                     let abi = []
                     let bytecode = ""
                     
                     if (tab.instanceId) {
                         const inst = deployedInstances.find(i => i.id === tab.instanceId)
                         if (inst) {
                             contractAddress = inst.address
                             abi = inst.artifact.abi
                             bytecode = inst.artifact.bytecode.object
                         }
                     } else {
                         // Fallback for generic deploy tabs
                         const contract = contracts.find(c => c.name === tab.contractName)
                         if (contract) {
                             // Generic contract doesn't have address yet
                             abi = contract.artifact.abi
                             bytecode = contract.artifact.bytecode.object
                         }
                     }

                     const contractArtifact = contracts.find(c => c.name === tab.contractName) // Still needed for deploy link

                     return (
                         <div 
                            key={tab.id} 
                            className={clsx("absolute inset-0 bg-slate-950", activeTabId === tab.id ? "z-10 block" : "z-0 hidden")}
                         >
                            {tab.type === 'details' ? (
                                <ContractDetailsTab 
                                    contractName={tab.contractName}
                                    contractAddress={contractAddress || null}
                                    abi={abi}
                                    bytecode={bytecode || ""}
                                    publicClient={clients.publicClient}
                                    walletClient={clients.walletClient}
                                    rpcUrl={rpcUrl}
                                    isActive={activeTabId === tab.id}
                                    globalMode={globalMode}
                                    ensureLocalClients={ensureLocalClients}
                                    localClients={localClients}
                                    onSnapshotCreated={addSnapshot}
                                    onSnapshotUpdated={updateSnapshot}
                                    snapshotsCount={snapshots.length}
                                    onLog={log}
                                    onDeploy={() => contractArtifact && openDeployTab(contractArtifact)}
                                />
                            ) : (
                                <RequestTab 
                                    type={tab.type as 'deploy' | 'function'}
                                    contractName={tab.contractName}
                                    abiItem={tab.abiItem}
                                    bytecode={bytecode}
                                    contractAddress={contractAddress || null}
                                    publicClient={clients.publicClient}
                                    walletClient={clients.walletClient}
                                    onLog={log}
                                    onDeploySuccess={(addr) => handleDeploySuccess(tab.contractName, addr)}
                                    onDeployRequest={() => contractArtifact && openDeployTab(contractArtifact)}
                                    rpcUrl={rpcUrl}
                                    globalMode={globalMode}
                                    ensureLocalClients={ensureLocalClients}
                                    localClients={localClients}
                                    onSnapshotCreated={addSnapshot}
                                    onSnapshotUpdated={updateSnapshot}
                                    snapshotsCount={snapshots.length}
                                />
                            )}
                         </div>
                     )
                 })}
                 {!clients && (
                     <div className="absolute inset-0 flex items-center justify-center text-red-500">
                         Invalid Configuration (Check Private Key/RPC)
                     </div>
                 )}
            </div>
        </div>
        </>
        ) : (
            <div className="flex-1 min-h-0">
                {clients ? (
                    <Explorer 
                        publicClient={globalMode === 'local' && localClients ? localClients.publicClient : clients.publicClient}
                        rpcUrl={globalMode === 'local' && localClients ? localClients.rpcUrl : rpcUrl}
                        txModes={txModes}
                        blockModes={blockModes}
                        onRemoveRecentBlock={(hash: string) => {
                          setRecentBlocks(prev => prev.filter(block => block.hash !== hash))
                          setBlockModes(prev => {
                            const next = { ...prev }
                            delete next[hash]
                            return next
                          })
                        }}
                        onRemoveRecentTx={(hash: string) => {
                          setRecentTransactions(prev => prev.filter(tx => tx.hash !== hash))
                          setTxModes(prev => {
                            const next = { ...prev }
                            delete next[hash]
                            return next
                          })
                        }}
                        onLog={log} 
                        recentBlocks={recentBlocks}
                        recentTransactions={recentTransactions}
                    />
                ) : (
                    <div className="flex items-center justify-center h-full text-slate-500">
                        Connecting to RPC...
                    </div>
                )}
            </div>
        )}
        </div>

        {/* Global Resizer & Bottom Panel */}
        <div 
            onMouseDown={startResizing}
            className="h-1 bg-slate-800 hover:bg-indigo-500 transition-colors cursor-row-resize shrink-0 z-20 relative"
        />

        <div style={{ height: bottomHeight }} className="shrink-0 min-h-[20px] relative z-20 bg-slate-900">
            <BottomPanel 
                logs={logs} 
                onClear={() => setLogs([])}
                snapshots={snapshots}
                activeSnapshotId={activeSnapshotId}
                onRevert={revertSnapshot}
            />
        </div>
      </div>
    </div>

    {updateToast && (
      <div className="fixed bottom-4 right-4 z-50">
        <button
          onClick={handleRefreshContracts}
          className="bg-slate-900 border border-slate-800 rounded-lg px-4 py-3 text-sm text-slate-200 shadow-lg hover:border-indigo-500 transition-colors flex items-center gap-3"
          title="Refresh contract UI"
        >
          <span>{updateToast.message}</span>
          <span className="text-indigo-400 text-xs font-bold uppercase">Refresh</span>
        </button>
      </div>
    )}
    {snapshotToast && (
      <div className="fixed bottom-4 left-4 z-50">
        <div
          className={clsx(
            "bg-slate-900 border rounded-lg px-4 py-3 text-sm shadow-lg",
            snapshotToast.status === "success"
              ? "border-emerald-700/60 text-emerald-200"
              : "border-red-700/60 text-red-200",
          )}
        >
          {snapshotToast.message}
        </div>
      </div>
    )}
    </>
  )
}

export default App
