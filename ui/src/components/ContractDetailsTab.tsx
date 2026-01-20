import { useEffect, useState, useMemo, useRef, useCallback } from 'react'
import { clsx } from 'clsx'
import { Copy, RefreshCw, Database, Code, FileJson, Search, Zap, Eye, EyeOff, Send, Loader2, RotateCcw, ChevronDown, ChevronUp, Activity } from 'lucide-react'
import type { Address, PublicClient, Hex, WalletClient, TestClient } from 'viem'
import { keccak256, toHex, pad, numberToHex, hexToBigInt, hexToBool, toBytes, formatEther, parseUnits, formatUnits } from 'viem'

import Editor from 'react-simple-code-editor'
import { Cheatcodes } from './Cheatcodes'
import { highlight, languages } from 'prismjs'
import 'prismjs/components/prism-json'
import 'prismjs/components/prism-clike'

interface ContractDetailsTabProps {
    contractName: string;
    contractAddress: Address | null;
    abi: any[];
    bytecode: string;
    publicClient: PublicClient;
    walletClient: WalletClient;
    rpcUrl?: string;
    isActive?: boolean;
    globalMode: 'live' | 'local';
    ensureLocalClients: () => Promise<{ publicClient: PublicClient; walletClient: WalletClient; testClient: TestClient; rpcUrl: string }>;
    localClients: { publicClient: PublicClient; walletClient: WalletClient; testClient: TestClient; rpcUrl: string } | null;
    onSnapshotCreated?: (entry: {
        snapshotId: string;
        method: string;
        from?: string;
        to?: string;
        value?: string;
    }) => string;
    onSnapshotUpdated?: (id: string, patch: { txHash?: string; blockNumber?: number; status?: 'confirmed' | 'error' }) => void;
    snapshotsCount?: number;
    onLog: (msg: string) => void;
    onDeploy: () => void;
}

interface StorageItem {
    astId: number;
    contract: string;
    label: string;
    offset: number;
    slot: string;
    type: string;
}

interface StorageType {
    encoding: string;
    label: string;
    numberOfBytes: string;
    base?: string; 
    key?: string; 
    value?: string; 
}

const COMMON_KEYS = [
    'transactionHash', 
    'hash', 
    'from', 
    'to', 
    'contractAddress', 
    'blockNumber', 
    'status', 
    'value', 
    'gasUsed', 
    'gasPrice'
];

type ValueUnit = 'wei' | 'gwei' | 'ether';

// Simple ETH Icon
const EthIcon = ({ size = 12, className }: { size?: number, className?: string }) => (
    <svg 
        xmlns="http://www.w3.org/2000/svg" 
        width={size} 
        height={size} 
        viewBox="0 0 32 32" 
        fill="currentColor"
        className={className}
    >
        <g fillRule="evenodd">
            <path d="M16 32C7.163 32 0 24.837 0 16S7.163 0 16 0s16 7.163 16 16-7.163 16-16 16zm7.994-15.781L16.498 4 9 16.22l7.498 4.353 7.496-4.354zM24 17.616l-7.502 4.351L9 17.617l7.498 10.378L24 17.616z" />
            <path d="M16.498 4v8.87l7.497 3.35L16.498 4zm0 6.643v5.855l7.497-4.351-7.497-1.504zM9 16.22l7.498 1.504v-5.855L9 16.22z" fillOpacity=".602" />
            <path d="M16.498 27.995v-6.028L9 17.616l7.498 10.379zm0-10.38l7.502-4.351-7.502 6.029v-1.678z" fillOpacity=".602" />
            <path d="M16.498 27.995l7.498-10.378-7.498 4.351v6.027z" fillOpacity=".2" />
            <path d="M9 16.22l7.498 4.353v-5.856L9 16.22z" fillOpacity=".2" />
            <path d="M16.498 11.548l-7.498-1.504L16.498 4v7.548z" fillOpacity=".2" />
        </g>
    </svg>
)

function UnitDisplay({ value }: { value: bigint }) {
    const [unit, setUnit] = useState<ValueUnit>('ether')

    const displayValue = useMemo(() => {
        try {
            return formatUnits(value, unit === 'wei' ? 0 : unit === 'gwei' ? 9 : 18)
        } catch {
            return value.toString()
        }
    }, [value, unit])

    const nextUnit = () => {
        setUnit(prev => prev === 'ether' ? 'gwei' : prev === 'gwei' ? 'wei' : 'ether')
    }

    return (
        <span 
            onClick={nextUnit} 
            className="cursor-pointer hover:bg-slate-800 rounded px-1 -mx-1 transition-colors select-none"
            title="Click to toggle unit"
        >
            {displayValue} <span className="text-slate-500 text-[10px] uppercase font-bold">{unit === 'ether' ? 'ETH' : unit}</span>
        </span>
    )
}

export function ContractDetailsTab({
    contractName, 
    contractAddress, 
    abi, 
    bytecode, 
    publicClient,
    walletClient,
    rpcUrl,
    isActive = false,
    globalMode,
    ensureLocalClients,
    localClients,
    onSnapshotCreated,
    onSnapshotUpdated,
    snapshotsCount = 0,
    onLog,
    onDeploy
}: ContractDetailsTabProps) {
    const [activeTab, setActiveTab] = useState<'storage' | 'bytecode' | 'abi' | 'interact'>('storage')
    const [storageLayout, setStorageLayout] = useState<{ storage: StorageItem[], types: Record<string, StorageType> } | null>(null)
    const [loadingLayout, setLoadingLayout] = useState(false)
    const [contractBalance, setContractBalance] = useState<bigint | null>(null)
    const [balanceUnit, setBalanceUnit] = useState<ValueUnit>('ether')
    
    // Storage State
    const [storageValues, setStorageValues] = useState<Record<string, string>>({})
    const [mappingKeys, setMappingKeys] = useState<Record<string, string>>({})
    const [viewModes, setViewModes] = useState<Record<string, 'decoded' | 'raw'>>({})

    // Interact State
    const [sendValue, setSendValue] = useState("")
    const [valueUnit, setValueUnit] = useState<ValueUnit>('ether')
    const [calldata, setCalldata] = useState("")
    const [gasLimit, setGasLimit] = useState("")
    const [isSending, setIsSending] = useState(false)
    const [interactResponse, setInteractResponse] = useState<any>(null)
    const [interactError, setInteractError] = useState<string | null>(null)
    const [responseViewMode, setResponseViewMode] = useState<'pretty' | 'raw' | 'trace'>('pretty')
    const [showAdvancedResponse, setShowAdvancedResponse] = useState(false)
    const [showCheatcodes, setShowCheatcodes] = useState(false)
    const [localTestClient, setLocalTestClient] = useState<TestClient | null>(null)
    const [localPublicClient, setLocalPublicClient] = useState<PublicClient | null>(null)
    const [isUnitDropdownOpen, setIsUnitDropdownOpen] = useState(false)
    const unitDropdownRef = useRef<HTMLDivElement>(null)
    const [traceData, setTraceData] = useState<string | null>(null)
    const [loadingTrace, setLoadingTrace] = useState(false)
    const pendingSnapshotIdRef = useRef<string | null>(null)
    
    // Interact Raw View State
    const [interactRequestMode, setInteractRequestMode] = useState<'form' | 'rpc'>('form')
    const [interactRawJson, setInteractRawJson] = useState("")

    const activePublicClient = globalMode === 'local'
        ? (localPublicClient ?? localClients?.publicClient ?? publicClient)
        : publicClient
    const activeWalletClient = globalMode === 'local'
        ? (localClients?.walletClient ?? walletClient)
        : walletClient

    const refreshBalance = useCallback(async () => {
        if (!contractAddress) return
        try {
            const bal = await activePublicClient.getBalance({ address: contractAddress })
            setContractBalance(bal)
        } catch (e) {
            console.error("Failed balance", e)
        }
    }, [contractAddress, activePublicClient])

    // Fetch once on open or when address/client changes
    useEffect(() => {
        refreshBalance()
    }, [refreshBalance])

    // Poll balance only when details tab is active
    useEffect(() => {
        if (!isActive) return
        refreshBalance()
        const interval = setInterval(refreshBalance, 5000)
        return () => clearInterval(interval)
    }, [isActive, refreshBalance])

    useEffect(() => {
        const handleClick = (e: MouseEvent) => {
            if (unitDropdownRef.current && !unitDropdownRef.current.contains(e.target as Node)) {
                setIsUnitDropdownOpen(false)
            }
        }
        document.addEventListener('mousedown', handleClick)
        return () => document.removeEventListener('mousedown', handleClick)
    }, [])

    useEffect(() => {
        if (globalMode !== 'local') {
            setLocalTestClient(null)
            setLocalPublicClient(null)
            return
        }
        let mounted = true
        if (localClients?.publicClient) {
            setLocalPublicClient(localClients.publicClient)
        }
        if (localClients?.testClient) {
            setLocalTestClient(localClients.testClient)
        }
        ensureLocalClients()
            .then((clients) => {
                if (!mounted) return
                setLocalTestClient(clients.testClient)
                setLocalPublicClient(clients.publicClient)
            })
            .catch(() => {})
        return () => {
            mounted = false
        }
    }, [globalMode, ensureLocalClients, localClients])

    // Fetch Layout
    useEffect(() => {
        const fetchLayout = async () => {
            setLoadingLayout(true)
            try {
                const res = await fetch(`http://localhost:3000/inspect/${contractName}`)
                const data = await res.json()
                if (data.storage) {
                    setStorageLayout(data)
                } else {
                    onLog(`No storage layout found for ${contractName}. (Forge output: ${JSON.stringify(data)})`)
                }
            } catch (e) {
                console.error(e)
                onLog(`Failed to fetch storage layout: ${e}`)
            } finally {
                setLoadingLayout(false)
            }
        }
        fetchLayout()
    }, [contractName])

    const getSendValueInWei = useCallback(() => {
        if (!sendValue) return undefined;
        try {
            return parseUnits(sendValue, valueUnit === 'wei' ? 0 : valueUnit === 'gwei' ? 9 : 18);
        } catch {
            return undefined;
        }
    }, [sendValue, valueUnit]);

    const handleUnitChange = (newUnit: ValueUnit) => {
        if (!sendValue) {
            setValueUnit(newUnit);
            setIsUnitDropdownOpen(false);
            return;
        }
        try {
            const currentWei = getSendValueInWei();
            if (currentWei !== undefined) {
                const newValue = formatUnits(currentWei, newUnit === 'wei' ? 0 : newUnit === 'gwei' ? 9 : 18);
                setSendValue(newValue);
            }
            setValueUnit(newUnit);
        } catch (e) {
            console.error("Conversion failed", e);
            setValueUnit(newUnit);
        }
        setIsUnitDropdownOpen(false);
    };

    const generateInteractRawJson = useCallback(() => {
        try {
            const valBigInt = getSendValueInWei();
            const gas = gasLimit ? BigInt(gasLimit) : undefined
            const jsonObj = {
                method: 'eth_sendTransaction',
                params: [{
                    to: contractAddress,
                    from: activeWalletClient.account?.address,
                    data: calldata || '0x',
                    value: valBigInt ? `0x${valBigInt.toString(16)}` : '0x0',
                    gas: gas ? `0x${gas.toString(16)}` : undefined
                }]
            }
            return JSON.stringify(jsonObj, (_, v) => typeof v === 'bigint' ? `0x${v.toString(16)}` : v, 2)
        } catch (e) { return "" }
    }, [getSendValueInWei, gasLimit, calldata, contractAddress, activeWalletClient.account])

    useEffect(() => {
        if (interactRequestMode === 'form') {
            const json = generateInteractRawJson()
            if (json) setInteractRawJson(json)
        }
    }, [generateInteractRawJson, interactRequestMode])

    const handleInteractResetRaw = () => {
        const json = generateInteractRawJson()
        if (json) setInteractRawJson(json)
    }

    const handleInteractViewSwitch = (mode: 'form' | 'rpc') => {
        if (mode === 'form' && interactRequestMode === 'rpc') {
            try {
                const obj = JSON.parse(interactRawJson)
                const params = obj.params?.[0] || {}
                
                if (params.value) {
                    const valWei = BigInt(params.value)
                    setSendValue(formatUnits(valWei, valueUnit === 'wei' ? 0 : valueUnit === 'gwei' ? 9 : 18))
                }
                if (params.gas) {
                    setGasLimit(BigInt(params.gas).toString())
                }
                if (params.data && params.data !== '0x') {
                    setCalldata(params.data)
                }
            } catch (e) {
                console.error("Failed to sync raw to form", e)
            }
        }
        setInteractRequestMode(mode)
    }

    const isArrayLabel = (label: string) => label.includes('[') && label.endsWith(']')

    const parseArrayLabel = (label: string) => {
        const match = label.match(/^(.*)\[(\d*)\]$/)
        if (!match) return { baseLabel: label, length: null as number | null }
        const length = match[2] ? Number(match[2]) : null
        return { baseLabel: match[1], length: Number.isNaN(length) ? null : length }
    }

    const isDynamicType = (typeLabel: string, typeDef?: StorageType) => {
        if (typeLabel === 'string' || typeLabel === 'bytes') return true
        if (typeDef?.encoding === 'dynamic_array') return true
        return false
    }

    const slotHash = (slotIndex: bigint) => {
        const slotHex = pad(numberToHex(slotIndex), { size: 32 })
        return hexToBigInt(keccak256(toBytes(slotHex)))
    }

    const decodeByLabel = (raw: string, label: string) => {
        if (!raw || raw.startsWith('Error')) return raw
        if (label.includes('uint') || label.includes('int')) {
            try {
                return hexToBigInt(raw as Hex).toString()
            } catch {
                return raw
            }
        }
        if (label.includes('address')) {
            return '0x' + raw.replace(/^0x/, '').slice(-40)
        }
        if (label.includes('bool')) {
            return hexToBool(raw as Hex) ? 'true' : 'false'
        }
        return raw
    }

    const formatArrayDisplay = (value: any, numeric: boolean): string => {
        if (!Array.isArray(value)) {
            if (numeric && typeof value === 'string' && /^-?\d+$/.test(value)) {
                return value
            }
            return JSON.stringify(value)
        }
        const items = value.map((item) => formatArrayDisplay(item, numeric))
        return `[${items.join(', ')}]`
    }

    const readSolidityBytes = async (slotIndex: bigint): Promise<string> => {
        if (!contractAddress) return "0x"
        const slotValue = await activePublicClient.getStorageAt({
            address: contractAddress,
            slot: pad(numberToHex(slotIndex), { size: 32 })
        }) || "0x0"
        const slotBigInt = hexToBigInt(slotValue as Hex)
        const lsb = Number(slotBigInt & 1n)
        if (lsb === 1) {
            const len = Number((slotBigInt - 1n) / 2n)
            const bytes = new Uint8Array(len)
            const dataBytes = toBytes(slotValue as Hex)
            bytes.set(dataBytes.slice(0, Math.min(len, 31)))
            return toHex(bytes)
        }
        const len = Number(slotBigInt * 2n)
        const baseSlot = slotHash(slotIndex)
        const bytes = new Uint8Array(len)
        let offset = 0
        for (let i = 0; offset < len; i++) {
            const data = await activePublicClient.getStorageAt({
                address: contractAddress,
                slot: pad(numberToHex(baseSlot + BigInt(i)), { size: 32 })
            }) || "0x0"
            const chunk = toBytes(data as Hex)
            const take = Math.min(32, len - offset)
            bytes.set(chunk.slice(0, take), offset)
            offset += take
        }
        return toHex(bytes)
    }

    const slotsForType = (typeId: string): number => {
        const def = storageLayout?.types[typeId]
        if (!def) return 1
        if (isArrayLabel(def.label) && def.base) {
            const { length } = parseArrayLabel(def.label)
            if (def.encoding === 'dynamic_array' || length === null) return 1
            return (length || 1) * slotsForType(def.base)
        }
        if (def.label === 'string' || def.label === 'bytes') return 1
        return 1
    }

    const readTypeValue = async (slotIndex: bigint, typeId: string): Promise<any> => {
        const def = storageLayout?.types[typeId]
        if (!def) {
            const val = await activePublicClient.getStorageAt({
                address: contractAddress!,
                slot: pad(numberToHex(slotIndex), { size: 32 })
            })
            return val || "0x0"
        }

        if (isArrayLabel(def.label) && def.base) {
            const { length } = parseArrayLabel(def.label)
            let arrayLength = length
            let baseSlot = slotIndex
            if (def.encoding === 'dynamic_array' || length === null) {
                const rawLen = await activePublicClient.getStorageAt({
                    address: contractAddress!,
                    slot: pad(numberToHex(slotIndex), { size: 32 })
                }) || "0x0"
                arrayLength = Number(hexToBigInt(rawLen as Hex))
                baseSlot = slotHash(slotIndex)
            }
            const baseDef = storageLayout?.types[def.base]
            const elementSlots = baseDef && !isDynamicType(baseDef.label, baseDef) ? slotsForType(def.base) : 1
            const items = []
            for (let i = 0; i < (arrayLength || 0); i++) {
                const elementSlot = baseSlot + BigInt(i * elementSlots)
                const val = await readTypeValue(elementSlot, def.base)
                items.push(val)
            }
            return items
        }

        if (def.label === 'string') {
            return await readSolidityString(slotIndex)
        }
        if (def.label === 'bytes') {
            return await readSolidityBytes(slotIndex)
        }

        const raw = await activePublicClient.getStorageAt({
            address: contractAddress!,
            slot: pad(numberToHex(slotIndex), { size: 32 })
        }) || "0x0"
        return decodeByLabel(raw, def.label)
    }

    const readStorage = async (item: StorageItem) => {
        if (!contractAddress) return
        
        try {
            const typeDef = storageLayout?.types[item.type]
            let slot = BigInt(item.slot)
            
            if (typeDef?.encoding === 'mapping') {
                const keyInput = mappingKeys[item.label]
                if (!keyInput) {
                    setStorageValues(p => ({...p, [item.label]: "Enter a key"}))
                    return
                }
                
                const keyType = storageLayout?.types[typeDef.key!]?.label
                let encodedKey: `0x${string}`
                
                if (keyType?.includes('address')) {
                    encodedKey = pad(keyInput as `0x${string}`, { size: 32 })
                } else if (keyType?.includes('uint')) {
                    encodedKey = pad(numberToHex(BigInt(keyInput)), { size: 32 })
                } else {
                    encodedKey = pad(toHex(keyInput), { size: 32 })
                }

                const slotHex = pad(numberToHex(slot), { size: 32 })
                const keyBytes = toBytes(encodedKey)
                const slotBytes = toBytes(slotHex)
                const combined = new Uint8Array(keyBytes.length + slotBytes.length)
                combined.set(keyBytes)
                combined.set(slotBytes, keyBytes.length)
                
                slot = BigInt(keccak256(combined))
            }

            const label = typeDef?.label || ''
            if (isArrayLabel(label) && typeDef?.base) {
                const values = await readTypeValue(slot, item.type)
                const baseLabel = storageLayout?.types[typeDef.base]?.label || ''
                const numeric = baseLabel.includes('uint') || baseLabel.includes('int')
                const formatted = formatArrayDisplay(values, numeric)
                setStorageValues(p => ({...p, [item.label]: formatted}))
                return
            }
            if (label === 'string') {
                const strVal = await readSolidityString(slot)
                setStorageValues(p => ({...p, [item.label]: strVal}))
                return 
            }
            if (label === 'bytes') {
                const bytesVal = await readSolidityBytes(slot)
                setStorageValues(p => ({...p, [item.label]: bytesVal}))
                return
            }

            const val = await activePublicClient.getStorageAt({
                address: contractAddress,
                slot: pad(numberToHex(slot), { size: 32 })
            })
            
            setStorageValues(p => ({...p, [item.label]: val || "0x0"}))
        } catch (e: any) {
            setStorageValues(p => ({...p, [item.label]: `Error: ${e.message}`}))
        }
    }

    const readSolidityString = async (slotIndex: bigint): Promise<string> => {
        if (!contractAddress) return ""
        const slotValue = await activePublicClient.getStorageAt({
            address: contractAddress,
            slot: pad(numberToHex(slotIndex), { size: 32 })
        }) || "0x0"
        const slotBigInt = hexToBigInt(slotValue as Hex)
        const lsb = Number(slotBigInt & 1n) 
        if (lsb === 0) {
            const length = Number((slotBigInt & 0xFFn)) / 2
            const bytes = toBytes(slotValue as Hex)
            const dataBytes = bytes.slice(0, 31) 
            const actualBytes = dataBytes.slice(0, length)
            return new TextDecoder().decode(actualBytes)
        } else {
            const length = Number((slotBigInt - 1n) / 2n)
            const startSlotHex = keccak256(pad(numberToHex(slotIndex), { size: 32 }))
            let startSlot = hexToBigInt(startSlotHex)
            const numSlots = Math.ceil(length / 32)
            let combinedBytes = new Uint8Array(0)
            for (let i = 0; i < numSlots; i++) {
                const chunkSlot = startSlot + BigInt(i)
                const chunkVal = await activePublicClient.getStorageAt({
                    address: contractAddress,
                    slot: pad(numberToHex(chunkSlot), { size: 32 })
                }) || "0x0"
                const chunkBytes = toBytes(chunkVal as Hex)
                const newCombined = new Uint8Array(combinedBytes.length + chunkBytes.length)
                newCombined.set(combinedBytes)
                newCombined.set(chunkBytes, combinedBytes.length)
                combinedBytes = newCombined
            }
            const finalBytes = combinedBytes.slice(0, length)
            return new TextDecoder().decode(finalBytes)
        }
    }

    const handleSendEth = async () => {
        if (!contractAddress) return
        setIsSending(true)
        setInteractError(null)
        setInteractResponse(null)
        setTraceData(null)
        
        const runWithClients = async (clients: { publicClient: PublicClient; walletClient: WalletClient; testClient?: TestClient | null; rpcUrl: string }) => {
            let snapshotEntryId: string | null = null
            const createSnapshotEntry = async (
                info: { method: string; from?: string; to?: string; value?: string },
                markPending = false,
                patch?: { txHash?: string; blockNumber?: number; status?: 'confirmed' | 'error' },
            ) => {
                if (globalMode !== 'local') return null
                if (!clients.testClient || !onSnapshotCreated) return null
                const snapshotId = await clients.testClient.snapshot()
                snapshotEntryId = onSnapshotCreated({
                    snapshotId,
                    method: info.method,
                    from: info.from,
                    to: info.to,
                    value: info.value,
                })
                if (markPending) {
                    pendingSnapshotIdRef.current = snapshotEntryId
                }
                if (snapshotEntryId && onSnapshotUpdated && patch) {
                    onSnapshotUpdated(snapshotEntryId, patch)
                }
                return snapshotEntryId
            }
            if (interactRequestMode === 'rpc') {
                const req = JSON.parse(interactRawJson)
                onLog(`Executing Raw ${req.method}...`)
                if (snapshotsCount === 0) {
                    const preBlock = await clients.publicClient.getBlockNumber()
                    await createSnapshotEntry({
                        method: req.method,
                        from: req.params?.[0]?.from,
                        to: req.params?.[0]?.to,
                        value: req.params?.[0]?.value,
                    }, true, { status: 'confirmed', blockNumber: Number(preBlock) })
                }
                const res = await (clients.walletClient as any).request(req)
                onLog(`Tx: ${res}`)
                setInteractResponse({ transactionHash: res, status: "pending" })
                const receipt = await clients.publicClient.waitForTransactionReceipt({ hash: res })
                const tx = await clients.publicClient.getTransaction({ hash: res })
                setInteractResponse({ ...receipt, ...tx })
                await createSnapshotEntry({
                    method: req.method,
                    from: req.params?.[0]?.from,
                    to: req.params?.[0]?.to,
                    value: req.params?.[0]?.value,
                }, false, {
                    txHash: res,
                    blockNumber: Number(receipt.blockNumber),
                    status: 'confirmed',
                })
                pendingSnapshotIdRef.current = null
                onLog(`Confirmed in block ${receipt.blockNumber}`)
            } else {
                const val = getSendValueInWei() || 0n
                const data = (calldata as Hex) || '0x'
                const gas = gasLimit ? BigInt(gasLimit) : undefined
                
                onLog(`Sending ${formatEther(val)} ETH to ${contractName}...`)

                if (snapshotsCount === 0) {
                    const preBlock = await clients.publicClient.getBlockNumber()
                    await createSnapshotEntry({
                        method: 'Send ETH',
                        from: clients.walletClient.account?.address,
                        to: contractAddress || undefined,
                        value: val ? `${formatEther(val)} ETH` : '0 ETH',
                    }, true, { status: 'confirmed', blockNumber: Number(preBlock) })
                }

                const txParams: any = {
                    to: contractAddress,
                    value: val,
                    data: data,
                    gas: gas,
                }
                if (clients.walletClient.account) {
                    txParams.account = clients.walletClient.account
                }
                const hash = await clients.walletClient.sendTransaction(txParams)
                
                onLog(`Tx Sent: ${hash}`)
                setInteractResponse({ transactionHash: hash, status: "pending" })
                
                const receipt = await clients.publicClient.waitForTransactionReceipt({ hash })
                const tx = await clients.publicClient.getTransaction({ hash })
                setInteractResponse({ ...receipt, ...tx })

                await createSnapshotEntry({
                    method: 'Send ETH',
                    from: clients.walletClient.account?.address,
                    to: contractAddress || undefined,
                    value: val ? `${formatEther(val)} ETH` : '0 ETH',
                }, false, {
                    txHash: hash,
                    blockNumber: Number(receipt.blockNumber),
                    status: 'confirmed',
                })
                pendingSnapshotIdRef.current = null
                
                onLog(`Transaction confirmed`)
                
                const bal = await clients.publicClient.getBalance({ address: contractAddress })
                setContractBalance(bal)
            }
        }
        
        try {
            if (globalMode === 'local') {
                const clients = await ensureLocalClients()
                await runWithClients(clients)
            } else {
                await runWithClients({ publicClient: activePublicClient, walletClient: activeWalletClient, rpcUrl: rpcUrl || '', testClient: null })
            }
        } catch (e: any) {
            console.error(e)
            setInteractError(e.message || String(e))
            if (pendingSnapshotIdRef.current && onSnapshotUpdated) {
                onSnapshotUpdated(pendingSnapshotIdRef.current, { status: 'error' })
                pendingSnapshotIdRef.current = null
            }
            onLog(`Error sending transaction: ${e.message}`)
        } finally {
            setIsSending(false)
        }
    }

    const fetchTrace = async () => {
        const txHash = interactResponse?.transactionHash || interactResponse?.hash
        if (!txHash) return
        setLoadingTrace(true)
        setResponseViewMode('trace')
        const runTrace = async (rpcUrlOverride: string) => {
            const res = await fetch(`http://localhost:3000/trace/${txHash}?rpc_url=${encodeURIComponent(rpcUrlOverride)}`)
            const data = await res.json()
            if (data.error) setTraceData(`Error: ${data.error}`)
            else setTraceData(data.stdout || data.stderr || "No trace output.")
        }
        try {
            if (globalMode === 'local') {
                const clients = localClients ?? (await ensureLocalClients())
                await runTrace(clients.rpcUrl)
            } else {
                await runTrace(rpcUrl || "http://127.0.0.1:8545")
            }
        } catch (e: any) {
            setTraceData(`Failed to fetch trace: ${e.message}`)
        } finally {
            setLoadingTrace(false)
        }
    }

    const decodeValue = (raw: string, typeDef?: StorageType) => {
        if (!raw || raw.startsWith('Error') || raw === "Enter a key") return raw
        if (raw.trim().startsWith('[') || raw.trim().startsWith('{')) return raw
        if (!raw.startsWith('0x')) return `"${raw}"` 
        if (raw === "0x0") return "0" 
        try {
            const type = typeDef?.label || ''
            const hex = raw as Hex
            if (type.includes('uint') || type.includes('int')) return hexToBigInt(hex).toString()
            if (type.includes('address')) return '0x' + raw.slice(-40)
            if (type.includes('bool')) return hexToBool(hex) ? 'true' : 'false'
            return raw 
        } catch (e) {
            return raw + " (Decode Failed)"
        }
    }

    const toggleViewMode = (label: string) => {
        setViewModes(p => ({ ...p, [label]: p[label] === 'raw' ? 'decoded' : 'raw' }))
    }

    const stripAnsi = (str: string) => str.replace(/\x1B\[[0-9;]*[a-zA-Z]/g, '')

    const toggleBalanceUnit = () => {
        setBalanceUnit(prev => prev === 'ether' ? 'gwei' : prev === 'gwei' ? 'wei' : 'ether')
    }

    const displayBalance = useMemo(() => {
        if (contractBalance === null) return '...'
        try {
            return formatUnits(contractBalance, balanceUnit === 'wei' ? 0 : balanceUnit === 'gwei' ? 9 : 18)
        } catch {
            return contractBalance.toString()
        }
    }, [contractBalance, balanceUnit])

    const renderPrettyResponse = () => {
        if (!interactResponse) return null
        if (typeof interactResponse !== 'object') return <div className="text-indigo-300 text-sm p-2 font-semibold">{String(interactResponse)}</div>

        const keys = Object.keys(interactResponse)
        const commonKeys = keys.filter(k => COMMON_KEYS.includes(k))
        const advancedKeys = keys.filter(k => !COMMON_KEYS.includes(k))

        const renderRow = (key: string, val: any) => {
            if (val === null || val === undefined) return null
            let content = <span className="text-indigo-300 break-all">{String(val)}</span>
            if (key === 'value' || key === 'gasPrice' || key === 'effectiveGasPrice') {
                try {
                    const bigVal = BigInt(val)
                    content = <UnitDisplay value={bigVal} />
                } catch {}
            }
            return (
                <div key={key} className="flex gap-2 border-b border-slate-800/30 pb-1.5 pt-1.5 last:border-0 hover:bg-slate-900/40 px-2 rounded -mx-2 transition-colors">
                    <span className="text-slate-500 min-w-[120px] font-medium">{key}:</span> 
                    {content}
                </div>
            )
        }

        return (
            <div className="space-y-1">
                <div className="space-y-0.5">
                    {commonKeys.map(k => renderRow(k, interactResponse[k]))}
                </div>
                {advancedKeys.length > 0 && (
                    <div className="pt-4">
                        <button 
                            onClick={() => setShowAdvancedResponse(!showAdvancedResponse)}
                            className="text-[10px] uppercase font-bold text-slate-500 hover:text-indigo-400 flex items-center gap-1 transition-colors"
                        >
                            {showAdvancedResponse ? "Hide" : "Show"} Advanced Fields
                            {showAdvancedResponse ? <ChevronUp size={12}/> : <ChevronDown size={12}/>}
                        </button>
                        {showAdvancedResponse && (
                             <div className="mt-2 pl-2 border-l-2 border-slate-800 space-y-0.5 animate-in fade-in slide-in-from-top-1 duration-200">
                                {advancedKeys.map(k => renderRow(k, interactResponse[k]))}
                             </div>
                        )}
                    </div>
                )}
            </div>
        )
    }

    const interactLineNumbers = useMemo(() => {
        const lines = interactRawJson.split('\n').length
        return Array.from({ length: lines }, (_, i) => i + 1)
    }, [interactRawJson])

    if (!contractAddress) {
        return (
            <div className="flex flex-col h-full bg-slate-950 items-center justify-center p-8 text-center space-y-6">
                <div className="bg-slate-900 p-6 rounded-full border border-slate-800">
                    <Database size={48} className="text-slate-700" />
                </div>
                <div className="space-y-2">
                    <h2 className="text-xl font-bold text-slate-200">Contract Not Deployed</h2>
                    <p className="text-slate-500 max-w-md">
                        Deploy <span className="text-indigo-400 font-mono">{contractName}</span> to inspect its live storage state, interact with functions, and view logs.
                    </p>
                </div>
                <button 
                    onClick={onDeploy}
                    className="px-8 py-3 bg-indigo-600 hover:bg-indigo-500 text-white font-bold rounded-lg shadow-lg shadow-indigo-900/20 flex items-center gap-2 transition-transform active:scale-95"
                >
                    <Zap size={18} fill="currentColor" /> Deploy Now
                </button>
                <div className="flex gap-4 pt-4">
                    <button onClick={() => setActiveTab('bytecode')} className="text-sm text-slate-500 hover:text-slate-300 underline underline-offset-4 decoration-slate-800">View Bytecode</button>
                    <button onClick={() => setActiveTab('abi')} className="text-sm text-slate-500 hover:text-slate-300 underline underline-offset-4 decoration-slate-800">View ABI</button>
                </div>
                {(activeTab === 'bytecode' || activeTab === 'abi') && (
                    <div className="absolute inset-0 bg-slate-950 z-10 flex flex-col">
                        <div className="p-4 border-b border-slate-800 flex justify-between items-center">
                            <h3 className="font-bold text-slate-400 uppercase tracking-wider">{activeTab}</h3>
                            <button onClick={() => setActiveTab('storage')} className="text-slate-500 hover:text-white">Close</button>
                        </div>
                        <div className="flex-1 p-6 overflow-auto text-left">
                             {activeTab === 'bytecode' && <textarea readOnly className="w-full h-full bg-transparent text-slate-400 font-mono text-xs resize-none focus:outline-none" value={bytecode} />}
                             {activeTab === 'abi' && <pre className="text-slate-300 font-mono text-xs">{JSON.stringify(abi, null, 2)}</pre>}
                        </div>
                    </div>
                )}
            </div>
        )
    }

    return (
        <div className="flex flex-col h-full bg-slate-950">
            <div className="px-6 py-4 border-b border-slate-800 flex justify-between items-start">
                <div>
                    <h2 className="text-lg font-bold text-slate-200">{contractName}</h2>
                    <div className="flex items-center gap-3 mt-1">
                        <div className="flex items-center gap-2 bg-slate-900 px-2 py-1 rounded border border-slate-800">
                            <span className="text-xs text-green-400 font-mono flex items-center gap-1">
                                <div className="w-1.5 h-1.5 bg-green-500 rounded-full animate-pulse"/>
                                {contractAddress}
                            </span>
                            <Copy size={12} className="text-slate-600 cursor-pointer hover:text-slate-400" onClick={() => navigator.clipboard.writeText(contractAddress)}/>
                        </div>
                        {contractBalance !== null && (
                            <div 
                                onClick={toggleBalanceUnit}
                                className="text-xs text-slate-400 flex items-center gap-1.5 bg-slate-900 px-2 py-1 rounded border border-slate-800 font-mono cursor-pointer hover:bg-slate-800 select-none transition-colors"
                                title="Click to toggle unit"
                            >
                                <EthIcon className="text-indigo-500" />
                                {displayBalance} {balanceUnit === 'ether' ? 'ETH' : balanceUnit.toUpperCase()}
                            </div>
                        )}
                        <button
                            onClick={refreshBalance}
                            className="text-slate-500 hover:text-slate-300 transition-colors"
                            title="Refresh balance"
                        >
                            <RefreshCw size={14} />
                        </button>
                    </div>
                </div>
                
                <div className="flex bg-slate-900 rounded p-1 gap-1">
                    {['storage', 'interact', 'bytecode', 'abi'].map(t => (
                        <button
                            key={t}
                            onClick={() => setActiveTab(t as any)}
                            className={clsx(
                                "px-3 py-1.5 text-xs font-medium rounded capitalize flex items-center gap-2 transition-all",
                                activeTab === t ? "bg-indigo-600 text-white shadow" : "text-slate-400 hover:text-slate-200 hover:bg-slate-800"
                            )}
                        >
                            {t === 'storage' && <Database size={14}/>}
                            {t === 'interact' && <Send size={14}/>}
                            {t === 'bytecode' && <Code size={14}/>}
                            {t === 'abi' && <FileJson size={14}/>}
                            {t}
                        </button>
                    ))}
                </div>
            </div>

            <div className="flex-1 overflow-hidden relative">
                {activeTab === 'storage' && (
                    <div className="h-full overflow-y-auto p-6">
                        {loadingLayout ? (
                            <div className="flex items-center justify-center h-full text-slate-500 gap-2"><RefreshCw className="animate-spin" size={16}/> Loading Storage Layout...</div>
                        ) : !storageLayout ? (
                            <div className="text-slate-500 text-center italic mt-10">Storage layout unavailable.<br/><span className="text-xs opacity-50">Ensure `forge` is installed and the project compiles via CLI.</span></div>
                        ) : (
                            <div className="space-y-4 max-w-4xl mx-auto">
                                {storageLayout.storage.map((item) => {
                                    const typeDef = storageLayout.types[item.type]
                                    const isMapping = typeDef?.encoding === 'mapping'
                                    const rawValue = storageValues[item.label]
                                    const mode = viewModes[item.label] || 'decoded'
                                    const isDecoded = mode === 'decoded'
                                    const displayValue = isDecoded ? decodeValue(rawValue, typeDef) : rawValue
                                    const isPublic = abi.some((f: any) => f.type === 'function' && f.name === item.label)
                                    return (
                                        <div key={item.label} className="bg-slate-900/50 border border-slate-800 rounded p-4 flex flex-col gap-2 hover:border-slate-700 transition-colors">
                                            <div className="flex items-center justify-between">
                                                <div className="flex items-center gap-2">
                                                    <span className="font-mono text-sm font-semibold text-indigo-400">{item.label}</span>
                                                    <span className={clsx("text-[9px] uppercase font-bold px-1.5 py-0.5 rounded border", isPublic ? "bg-blue-900/20 text-blue-400 border-blue-900/30" : "bg-red-900/20 text-red-400 border-red-900/30")}>{isPublic ? "Public" : "Private"}</span>
                                                    <span className="text-xs text-slate-600">({typeDef?.label})</span>
                                                    <span className="text-[10px] text-slate-500 bg-slate-900 px-1.5 rounded border border-slate-800">Slot {item.slot}</span>
                                                </div>
                                                <button onClick={() => readStorage(item)} className="text-xs bg-slate-800 hover:bg-indigo-600 hover:text-white text-slate-300 px-3 py-1 rounded flex items-center gap-1 transition-all"><Search size={12} /> Read</button>
                                            </div>
                                            {isMapping && (
                                                <div className="flex items-center gap-2 mt-1 bg-slate-950 p-1.5 rounded border border-slate-800">
                                                    <label className="text-[10px] uppercase font-bold text-slate-500 px-1">Key</label>
                                                    <input className="bg-transparent text-xs text-slate-200 focus:outline-none flex-1 font-mono placeholder:text-slate-700" placeholder={`Enter ${storageLayout.types[typeDef.key!]?.label || 'key'}`} value={mappingKeys[item.label] || ""} onChange={e => setMappingKeys(p => ({...p, [item.label]: e.target.value}))}/>
                                                </div>
                                            )}
                                            {rawValue && (
                                                <div className="mt-1 relative group">
                                                    <div className="p-2 bg-black/40 rounded border border-slate-800/50 font-mono text-xs text-emerald-400 break-all shadow-inner pr-8 min-h-[34px] flex items-center">{displayValue}</div>
                                                    <button onClick={() => toggleViewMode(item.label)} className="absolute right-2 top-1/2 -translate-y-1/2 text-slate-500 hover:text-white opacity-0 group-hover:opacity-100 transition-opacity" title={isDecoded ? "Show Raw Hex" : "Decode Value"}>{isDecoded ? <Eye size={14}/> : <EyeOff size={14}/>}</button>
                                                </div>
                                            )}
                                        </div>
                                    )
                                })}
                            </div>
                        )}
                    </div>
                )}

                {activeTab === 'interact' && (
                    <div className="flex flex-row h-full w-full">
                        {/* Request Section */}
                        <div className="w-1/2 border-r border-slate-800 flex flex-col min-h-0">
                            <div className="flex items-center justify-between px-4 py-2 bg-slate-900/50 border-b border-slate-800 h-[45px]">
                                <h3 className="text-xs font-bold text-slate-500 uppercase tracking-wider">Parameters</h3>
                                <div className="flex items-center gap-2">
                                    {interactRequestMode === 'rpc' && (
                                        <button onClick={handleInteractResetRaw} className="text-slate-500 hover:text-indigo-400 p-1 rounded transition-colors" title="Reset to Form Data"><RotateCcw size={12} /></button>
                                    )}
                                    <span className="text-[10px] uppercase text-slate-500 font-semibold">Mode</span>
                                    <span className={clsx(
                                        "text-[10px] uppercase font-bold px-2 py-1 rounded",
                                        globalMode === "local"
                                            ? "bg-indigo-600/30 text-indigo-200"
                                            : "bg-slate-800 text-slate-400",
                                    )}>
                                        {globalMode}
                                    </span>
                                    <div className="flex bg-slate-800 rounded p-0.5">
                                        <button onClick={() => handleInteractViewSwitch('form')} className={clsx("px-3 py-1 text-[10px] font-medium rounded-sm transition-all", interactRequestMode === 'form' ? "bg-slate-600 text-slate-100 shadow-sm" : "text-slate-400 hover:text-slate-300")}>Form</button>
                                        <button onClick={() => handleInteractViewSwitch('rpc')} className={clsx("px-3 py-1 text-[10px] font-medium rounded-sm transition-all", interactRequestMode === 'rpc' ? "bg-slate-600 text-slate-100 shadow-sm" : "text-slate-400 hover:text-slate-300")}>Raw</button>
                                    </div>
                                </div>
                            </div>

                            <div className="flex-1 overflow-y-auto p-6 flex flex-col">
                                {interactRequestMode === 'form' ? (
                                    <div className="max-w-xl mx-auto space-y-6 w-full">
                                        <div className="space-y-4">
                                            <div className="flex flex-col gap-1.5">
                                                <label className="text-xs font-medium text-slate-400">Value</label>
                                                <div className="flex gap-2 relative" ref={unitDropdownRef}>
                                                    <input className="flex-1 bg-transparent border-b border-slate-700 py-2 px-1 text-sm text-slate-200 focus:border-indigo-500 outline-none placeholder:text-slate-700" placeholder="0.0" type="number" value={sendValue} onChange={e => setSendValue(e.target.value)} />
                                                    <button onClick={() => setIsUnitDropdownOpen(!isUnitDropdownOpen)} className="bg-slate-900 border-b border-slate-700 text-xs text-slate-400 focus:outline-none focus:text-slate-200 cursor-pointer flex items-center gap-1 px-2 hover:bg-slate-800 transition-colors">{valueUnit.toUpperCase()} <ChevronDown size={10} /></button>
                                                    {isUnitDropdownOpen && (
                                                        <div className="absolute top-full right-0 mt-1 bg-slate-900 border border-slate-700 rounded shadow-lg z-50 w-20 overflow-hidden">
                                                            {['ether', 'gwei', 'wei'].map((unit) => (
                                                                <div key={unit} onClick={() => handleUnitChange(unit as ValueUnit)} className={clsx("px-3 py-1.5 text-xs cursor-pointer hover:bg-slate-800 transition-colors uppercase", valueUnit === unit ? "text-indigo-400 font-bold bg-slate-800/50" : "text-slate-400")}>{unit === 'ether' ? 'ETH' : unit}</div>
                                                            ))}
                                                        </div>
                                                    )}
                                                </div>
                                            </div>
                                            <div className="flex flex-col gap-1.5">
                                                <label className="text-xs font-medium text-slate-400">Calldata (Hex)</label>
                                                <div className="relative">
                                                    <textarea className="w-full bg-slate-900/50 border border-slate-700 rounded p-3 text-sm text-slate-200 focus:border-indigo-500 outline-none h-24 font-mono resize-none transition-colors" placeholder="0x..." value={calldata} onChange={e => setCalldata(e.target.value)} />
                                                </div>
                                            </div>
                                            <div className="flex flex-col gap-1.5">
                                                <label className="text-xs font-medium text-slate-400">Gas Limit</label>
                                                <input className="bg-transparent border-b border-slate-700 py-2 px-1 text-sm text-slate-200 focus:border-indigo-500 outline-none placeholder:text-slate-700" placeholder="Auto" type="number" value={gasLimit} onChange={e => setGasLimit(e.target.value)} />
                                            </div>
                                        </div>
                                        {globalMode === 'local' && (
                                            <div className="pt-6 border-t border-slate-800/60">
                                                <button
                                                    onClick={() => setShowCheatcodes(prev => !prev)}
                                                    className="text-[10px] uppercase font-bold text-slate-400 hover:text-indigo-300 flex items-center gap-2"
                                                >
                                                    {showCheatcodes ? "Hide" : "Show"} Cheatcodes
                                                    <ChevronDown size={12} className={clsx("transition-transform", showCheatcodes && "rotate-180")} />
                                                </button>
                                                {showCheatcodes && (
                                                    <div className="mt-4 border border-slate-800 rounded-lg overflow-hidden">
                                                        <Cheatcodes
                                                            publicClient={localPublicClient ?? localClients?.publicClient ?? publicClient}
                                                            testClient={localTestClient ?? localClients?.testClient}
                                                            onLog={onLog}
                                                            enabled={globalMode === 'local'}
                                                            embedded
                                                            mode="live"
                                                        />
                                                    </div>
                                                )}
                                            </div>
                                        )}
                                    </div>
                                ) : (
                                    <div className="flex-1 min-h-0 flex flex-col bg-slate-900 border border-slate-800 rounded relative overflow-hidden">
                                        <div className="flex flex-1 overflow-auto">
                                            <div className="bg-slate-950 w-10 text-right pr-3 pt-3 text-slate-600 font-mono text-[10px] select-none shrink-0 leading-5">
                                                {interactLineNumbers.map(i => <div key={i}>{i}</div>)}
                                            </div>
                                            <div className="flex-1 relative">
                                                <Editor value={interactRawJson} onValueChange={setInteractRawJson} highlight={code => highlight(code, languages.json, 'json')} padding={12} className="font-mono text-[10px] leading-5" style={{ backgroundColor: 'transparent', color: '#e2e8f0', minHeight: '100%' }} textareaClassName="focus:outline-none" />
                                            </div>
                                        </div>
                                        <button onClick={handleInteractResetRaw} className="absolute top-2 right-2 bg-slate-800 hover:bg-slate-700 text-slate-400 hover:text-white p-1.5 rounded shadow transition-all z-10" title="Reset to Form Data"><RotateCcw size={14} /></button>
                                    </div>
                                )}
                                
                                <div className="h-6" />
                            </div>
                            <div className="p-6 border-t border-slate-800 bg-slate-950">
                                <button onClick={handleSendEth} disabled={isSending} className="w-full bg-indigo-600 hover:bg-indigo-500 text-white font-bold py-2.5 rounded flex items-center justify-center gap-2 transition-all shadow-lg shadow-indigo-900/20 active:translate-y-0.5 disabled:opacity-50 disabled:active:translate-y-0">{isSending ? <Loader2 className="animate-spin" size={16}/> : <Send size={16}/>} {isSending ? "Sending..." : "Send Transaction"}</button>
                            </div>
                        </div>

                        {/* Response Section */}
                        <div className="w-1/2 bg-slate-950 flex flex-col min-h-0 border-l border-slate-800">
                            <div className="flex items-center justify-between px-4 py-2 bg-slate-900/50 border-b border-slate-800 h-[45px]">
                                <div className="flex items-center gap-4">
                                    <h3 className="text-xs font-bold text-slate-500 uppercase tracking-wider">Response</h3>
                                    {(interactResponse?.transactionHash || interactResponse?.hash) && (
                                        <button onClick={fetchTrace} disabled={loadingTrace} className={clsx("text-[10px] flex items-center gap-1 px-2 py-0.5 rounded border transition-colors", responseViewMode === 'trace' ? "bg-indigo-900/30 text-indigo-400 border-indigo-900/50" : "bg-slate-900 text-slate-400 border-slate-800 hover:text-white")}>{loadingTrace ? <Loader2 size={10} className="animate-spin"/> : <Activity size={10} />} Trace</button>
                                    )}
                                </div>
                                <div className="flex bg-slate-800 rounded p-0.5">
                                    <button onClick={() => setResponseViewMode('pretty')} className={clsx("px-3 py-1 text-[10px] font-medium rounded-sm transition-all", responseViewMode === 'pretty' ? "bg-slate-600 text-slate-100 shadow-sm" : "text-slate-400 hover:text-slate-300")}>Pretty</button>
                                    <button onClick={() => setResponseViewMode('raw')} className={clsx("px-3 py-1 text-[10px] font-medium rounded-sm transition-all", responseViewMode === 'raw' ? "bg-slate-600 text-slate-100 shadow-sm" : "text-slate-400 hover:text-slate-300")}>Raw</button>
                                </div>
                            </div>
                            <div className="flex-1 overflow-auto p-6 text-xs font-mono">
                                {interactError ? (
                                    <div className="text-red-400 bg-red-900/10 border border-red-900/30 p-4 rounded-md"><div className="font-bold mb-1">Error</div>{interactError}</div>
                                ) : null}
                                {!interactError && interactResponse && responseViewMode === 'pretty' && renderPrettyResponse()}
                                {!interactError && interactResponse && responseViewMode === 'raw' && (
                                    <pre className="text-green-400 bg-slate-900/30 p-4 rounded-lg overflow-auto">{JSON.stringify(interactResponse, (_, v) => typeof v === 'bigint' ? v.toString() : v, 2)}</pre>
                                )}
                                {!interactError && traceData && responseViewMode === 'trace' && (
                                    <div className="relative h-full flex flex-col"><div className="flex-1 overflow-auto bg-slate-900/50 p-4 rounded border border-slate-800"><Editor value={stripAnsi(traceData)} onValueChange={() => {}} highlight={code => highlight(code, languages.clike, 'clike')} padding={12} className="font-mono text-[10px] leading-relaxed" style={{ fontFamily: 'monospace', backgroundColor: 'transparent', color: '#e2e8f0', minHeight: '100%' }} textareaClassName="focus:outline-none" readOnly /></div></div>
                                )}
                                {!interactError && !interactResponse && (
                                    <div className="h-full flex flex-col items-center justify-center text-slate-700 gap-2"><div className="w-12 h-12 rounded-full bg-slate-900 flex items-center justify-center"><div className="w-2 h-2 bg-slate-800 rounded-full animate-pulse" /></div><p>Ready to send transaction</p></div>
                                )}
                            </div>
                        </div>
                    </div>
                )}

                {activeTab === 'bytecode' && (
                    <div className="h-full p-6">
                        <textarea readOnly className="w-full h-full bg-slate-900 border border-slate-800 rounded p-4 font-mono text-xs text-slate-400 focus:outline-none resize-none" value={bytecode} />
                    </div>
                )}

                {activeTab === 'abi' && (
                    <div className="h-full p-6 overflow-auto">
                        <pre className="text-xs font-mono text-slate-300 bg-slate-900 p-4 rounded border border-slate-800">{JSON.stringify(abi, null, 2)}</pre>
                    </div>
                )}
            </div>
        </div>
    )
}