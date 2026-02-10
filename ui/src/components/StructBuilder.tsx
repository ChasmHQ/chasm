import { useState, useEffect } from 'react'
import { Plus, Trash2, ChevronRight, ChevronDown, Copy, Check, List } from 'lucide-react'
import { clsx } from 'clsx'

type SolidityType = 'string' | 'uint' | 'int' | 'address' | 'bool' | 'bytes' | 'tuple'

interface BuilderNode {
    id: string;
    name: string; 
    type: SolidityType;
    value: string;
    isArray: boolean;
    children: BuilderNode[]; 
}

export function StructBuilder() {
    const [root, setRoot] = useState<BuilderNode>({
        id: 'root',
        name: 'root',
        type: 'tuple', // Root wrapper is usually a tuple of args
        value: '',
        isArray: true, // Start as Array to match Utils.MultiPath[] example
        children: []
    })

    const [jsonOutput, setJsonOutput] = useState('')
    const [isCopied, setIsCopied] = useState(false)

    useEffect(() => {
        const parseNode = (node: BuilderNode): any => {
            // 1. Handle Array
            if (node.isArray) {
                return node.children.map(child => parseNode(child))
            }
            
            // 2. Handle Tuple (Struct) -> Return Array (Tuple Format)
            if (node.type === 'tuple') {
                // Map children to array values directly. Order matters!
                return node.children.map(child => parseNode(child))
            }
            
            // 3. Handle Primitives
            if (node.type === 'bool') return node.value === 'true'
            if (node.type === 'uint' || node.type === 'int') {
                return node.value // Keep as string for safety
            }
            return node.value
        }

        try {
            const result = parseNode(root)
            // Use 2 space indent for readability, but can be compacted
            setJsonOutput(JSON.stringify(result, null, 2))
        } catch (e) {
            setJsonOutput('Error parsing structure')
        }
    }, [root])

    // Helper to find and update a node in the tree
    const updateTree = (node: BuilderNode, targetId: string, fn: (n: BuilderNode) => BuilderNode): BuilderNode => {
        if (node.id === targetId) {
            return fn(node)
        }
        return { ...node, children: node.children.map(c => updateTree(c, targetId, fn)) }
    }

    const addField = (parentId: string) => {
        const newNode: BuilderNode = {
            id: Math.random().toString(36).substr(2, 9),
            name: `field_${Math.floor(Math.random() * 1000)}`,
            type: 'uint', // default to uint as it's common
            value: '',
            isArray: false,
            children: []
        }

        setRoot(prev => updateTree(prev, parentId, (node) => ({
            ...node,
            children: [...node.children, newNode]
        })))
    }

    const updateNode = (id: string, patch: Partial<BuilderNode>) => {
        setRoot(prev => updateTree(prev, id, (node) => ({ ...node, ...patch })))
    }

    const removeNode = (id: string) => {
        const recursiveRemove = (node: BuilderNode): BuilderNode => {
            return { 
                ...node, 
                children: node.children.filter(c => c.id !== id).map(recursiveRemove) 
            }
        }
        setRoot(prev => recursiveRemove(prev))
    }

    const copyToClipboard = () => {
        navigator.clipboard.writeText(jsonOutput)
        setIsCopied(true)
        setTimeout(() => setIsCopied(false), 2000)
    }

    return (
        <div className="flex flex-col h-full gap-4">
            <div className="flex-1 overflow-auto bg-slate-900 border border-slate-700 rounded-lg p-4">
                <NodeItem 
                    node={root} 
                    onAdd={addField} 
                    onUpdate={updateNode} 
                    onRemove={removeNode} 
                    isRoot 
                />
            </div>
            
            <div className="h-48 bg-slate-950 border border-slate-800 rounded-lg flex flex-col">
                <div className="flex items-center justify-between px-4 py-2 border-b border-slate-800 bg-slate-900/50">
                    <span className="text-xs font-bold text-slate-500 uppercase">Tuple Output (JSON)</span>
                    <button 
                        onClick={copyToClipboard}
                        className={clsx(
                            "flex items-center gap-1.5 px-2 py-1 rounded text-[10px] transition-colors",
                            isCopied ? "text-green-400 bg-green-900/20" : "text-slate-400 hover:text-slate-200 hover:bg-slate-800"
                        )}
                    >
                        {isCopied ? <Check size={12} /> : <Copy size={12} />}
                        {isCopied ? 'Copied' : 'Copy'}
                    </button>
                </div>
                <pre className="flex-1 p-4 overflow-auto font-mono text-xs text-green-300 whitespace-pre-wrap">
                    {jsonOutput}
                </pre>
            </div>
        </div>
    )
}

function NodeItem({ node, onAdd, onUpdate, onRemove, isRoot = false, isParentArray = false }: { 
    node: BuilderNode, 
    onAdd: (id: string) => void, 
    onUpdate: (id: string, patch: Partial<BuilderNode>) => void, 
    onRemove: (id: string) => void,
    isRoot?: boolean,
    isParentArray?: boolean
}) {
    const [expanded, setExpanded] = useState(true)
    const types: SolidityType[] = ['uint', 'address', 'string', 'bool', 'bytes', 'tuple', 'int']

    const isContainer = node.type === 'tuple' || node.isArray

    return (
        <div className={clsx("flex flex-col gap-2", !isRoot && "pl-4 border-l-2 border-slate-800 ml-1")}>
            {!isRoot && (
                <div className="flex items-center gap-2 group bg-slate-900/50 p-1 rounded hover:bg-slate-800/50 transition-colors">
                    {/* Collapser */}
                    {isContainer ? (
                        <button onClick={() => setExpanded(!expanded)} className="text-slate-500 hover:text-indigo-400 p-0.5">
                            {expanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
                        </button>
                    ) : (
                        <div className="w-4" /> 
                    )}
                    
                    {/* Field Name - Only purely cosmetic now for Tuple builder, but useful for user organization */}
                    {!isParentArray && (
                        <>
                            <input 
                                value={node.name}
                                onChange={(e) => onUpdate(node.id, { name: e.target.value })}
                                className="bg-transparent border border-transparent hover:border-slate-700 focus:border-indigo-500 rounded px-1 py-0.5 text-xs text-indigo-300 w-24 font-mono outline-none placeholder-slate-700"
                                placeholder="label"
                            />
                            <span className="text-slate-600">:</span>
                        </>
                    )}
                    
                    {/* Type Selector */}
                    <div className="relative flex items-center bg-slate-950 border border-slate-700 rounded px-1">
                        <select 
                            value={node.type}
                            onChange={(e) => onUpdate(node.id, { type: e.target.value as SolidityType })}
                            className="bg-transparent text-[10px] text-yellow-500 outline-none appearance-none pr-4 py-0.5 cursor-pointer"
                        >
                            {types.map(t => <option key={t} value={t}>{t}</option>)}
                        </select>
                        <ChevronDown size={10} className="absolute right-1 text-slate-600 pointer-events-none" />
                    </div>

                    {/* Array Toggle */}
                    <label 
                        className={clsx(
                            "flex items-center gap-1 cursor-pointer px-1.5 py-0.5 rounded border transition-colors select-none",
                            node.isArray ? "bg-indigo-900/30 border-indigo-500/50 text-indigo-300" : "border-transparent text-slate-600 hover:text-slate-400"
                        )}
                        title="Toggle Array"
                    >
                        <input 
                            type="checkbox" 
                            checked={node.isArray}
                            onChange={(e) => onUpdate(node.id, { isArray: e.target.checked })}
                            className="hidden" 
                        />
                        <List size={12} />
                        <span className="text-[10px] font-medium">{node.isArray ? '[]' : ''}</span>
                    </label>

                    {/* Value Input (for primitives) */}
                    {!isContainer && (
                        node.type === 'bool' ? (
                            <select 
                                value={node.value}
                                onChange={(e) => onUpdate(node.id, { value: e.target.value })}
                                className="bg-slate-950 border border-slate-700 rounded px-2 py-0.5 text-xs text-emerald-400 outline-none"
                            >
                                <option value="">(empty)</option>
                                <option value="true">true</option>
                                <option value="false">false</option>
                            </select>
                        ) : (
                            <input 
                                value={node.value}
                                onChange={(e) => onUpdate(node.id, { value: e.target.value })}
                                className="bg-slate-950 border border-slate-700 rounded px-2 py-0.5 text-xs text-slate-200 outline-none flex-1 min-w-[100px]"
                                placeholder="value"
                            />
                        )
                    )}

                    <div className="flex-1" />

                    <button onClick={() => onRemove(node.id)} className="text-slate-700 hover:text-red-400 transition-colors p-1">
                        <Trash2 size={12} />
                    </button>
                </div>
            )}

            {/* Container Body (Children) */}
            {(isRoot || (isContainer && expanded)) && (
                <div className="flex flex-col gap-1">
                    {node.children.map(child => (
                        <NodeItem 
                            key={child.id} 
                            node={child} 
                            onAdd={onAdd} 
                            onUpdate={onUpdate} 
                            onRemove={onRemove}
                            isParentArray={node.isArray} 
                        />
                    ))}
                    
                    <button 
                        onClick={() => onAdd(node.id)}
                        className="flex items-center gap-1.5 text-[10px] font-medium text-slate-500 hover:text-indigo-400 transition-colors w-full py-1.5 rounded hover:bg-slate-800/30 border border-dashed border-slate-800 hover:border-indigo-500/30 justify-center mt-1"
                    >
                        <Plus size={12} /> 
                        {node.isArray ? 'Add Item' : 'Add Field'}
                    </button>
                </div>
            )}
        </div>
    )
}