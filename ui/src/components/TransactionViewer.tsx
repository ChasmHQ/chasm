import { useState, useMemo } from "react";
import { formatUnits } from "viem";
import { clsx } from "clsx";
import { ChevronUp, ChevronDown, Activity, Loader2 } from "lucide-react";
import Editor from "react-simple-code-editor";
import { highlight, languages } from "prismjs";
import "prismjs/components/prism-json";
import "prismjs/components/prism-clike";

type ValueUnit = "wei" | "gwei" | "ether";

interface UnitDisplayProps {
  value: bigint;
}

export function UnitDisplay({ value }: UnitDisplayProps) {
  const [unit, setUnit] = useState<ValueUnit>("ether");

  const displayValue = useMemo(() => {
    try {
      return formatUnits(value, unit === "wei" ? 0 : unit === "gwei" ? 9 : 18);
    } catch {
      return value.toString();
    }
  }, [value, unit]);

  const nextUnit = () => {
    setUnit((prev) =>
      prev === "ether" ? "gwei" : prev === "gwei" ? "wei" : "ether"
    );
  };

  return (
    <span
      onClick={(e) => {
        e.stopPropagation();
        nextUnit();
      }}
      className="cursor-pointer hover:bg-slate-800 rounded px-1 -mx-1 transition-colors select-none"
      title="Click to toggle unit"
    >
      {displayValue}{" "}
      <span className="text-slate-500 text-[10px] uppercase font-bold">
        {unit === "ether" ? "ETH" : unit}
      </span>
    </span>
  );
}

interface TransactionViewerProps {
  data: any;
  onTrace?: (hash: string) => void;
  isLoadingTrace?: boolean;
  traceData?: string | null;
  className?: string;
  onNavigate?: (type: 'tx' | 'block' | 'address', value: string) => void;
}

const COMMON_KEYS = [
  "status",
  "transactionHash",
  "blockNumber",
  "gasUsed",
  "from",
  "to",
  "value",
  "hash",
  "gasPrice",
];

export function TransactionViewer({
  data,
  onTrace,
  isLoadingTrace = false,
  traceData,
  className,
  onNavigate,
}: TransactionViewerProps) {
  const [viewMode, setViewMode] = useState<"pretty" | "raw" | "trace">("pretty");
  const [showAdvanced, setShowAdvanced] = useState(false);

  // If traceData becomes available, auto-switch to trace view
  // But only if we requested it (tracked via isLoadingTrace transition?)
  // Actually, we can just let user switch or if traceData changes.
  // We'll add a 'Trace' button in header that switches viewMode.

  const stripAnsi = (str: string) => str.replace(/\x1B\[[0-9;]*[a-zA-Z]/g, "");

  const renderPretty = () => {
    if (!data) return null;
    const keys = Object.keys(data);
    
    // We want common keys in the specific order defined in COMMON_KEYS, if they exist in data.
    const commonKeys = COMMON_KEYS.filter(k => keys.includes(k));
    
    // Advanced keys are everything else in data, sorted alphabetically? or just as is.
    const advancedKeys = keys.filter((k) => !COMMON_KEYS.includes(k));

    const renderRow = (key: string, val: any) => {
      if (val === null || val === undefined) return null;

      let content = (
        <span className="text-indigo-300 break-all">{String(val)}</span>
      );

      if (key === 'status') {
          content = (
              <span className={clsx("px-2 py-0.5 rounded text-[10px] font-bold uppercase", val === 'success' || val === '0x1' ? "bg-green-900/30 text-green-400" : "bg-red-900/30 text-red-400")}>
                  {val === '0x1' ? 'Success' : val === '0x0' ? 'Reverted' : val}
              </span>
          )
      } else if (key === 'from' || key === 'to' || key === 'contractAddress') {
          content = (
              <span 
                onClick={() => onNavigate?.('address', String(val))}
                className="text-indigo-400 hover:text-indigo-300 cursor-pointer hover:underline break-all"
              >
                  {String(val)}
              </span>
          )
      } else if (key === 'blockNumber') {
          content = (
              <span 
                onClick={() => onNavigate?.('block', String(val))}
                className="text-indigo-400 hover:text-indigo-300 cursor-pointer hover:underline"
              >
                  {String(val)}
              </span>
          )
      } else if (
        key === "value" ||
        key === "gasPrice" ||
        key === "effectiveGasPrice" ||
        key === "maxFeePerGas" ||
        key === "maxPriorityFeePerGas"
      ) {
        try {
          const bigVal = BigInt(val);
          content = <UnitDisplay value={bigVal} />;
        } catch {}
      }

      return (
        <div
          key={key}
          className="flex gap-2 border-b border-slate-800/30 pb-1.5 pt-1.5 last:border-0 hover:bg-slate-900/40 px-2 rounded -mx-2 transition-colors"
        >
          <span className="text-slate-500 min-w-[140px] font-medium shrink-0">
            {key}:
          </span>
          {content}
        </div>
      );
    };

    return (
      <div className="space-y-1 text-sm font-mono">
        <div className="space-y-0.5">
          {commonKeys.map((k) => renderRow(k, data[k]))}
        </div>
        {advancedKeys.length > 0 && (
          <div className="pt-4">
            <button
              onClick={() => setShowAdvanced(!showAdvanced)}
              className="text-[10px] uppercase font-bold text-slate-500 hover:text-indigo-400 flex items-center gap-1 transition-colors"
            >
              {showAdvanced ? "Hide" : "Show"} Advanced Fields
              {showAdvanced ? (
                <ChevronUp size={12} />
              ) : (
                <ChevronDown size={12} />
              )}
            </button>
            {showAdvanced && (
              <div className="mt-2 pl-2 border-l-2 border-slate-800 space-y-0.5 animate-in fade-in slide-in-from-top-1 duration-200">
                {advancedKeys.map((k) => renderRow(k, data[k]))}
              </div>
            )}
          </div>
        )}
      </div>
    );
  };

  const txHash = data?.transactionHash || data?.hash;

  return (
    <div className={clsx("flex flex-col min-h-0 h-full", className)}>
      {/* Header */}
      <div className="flex items-center justify-between px-4 py-2 bg-slate-900/50 border-b border-slate-800 h-[45px] shrink-0">
        <div className="flex items-center gap-4">
          <h3 className="text-xs font-bold text-slate-500 uppercase tracking-wider">
            Transaction Details
          </h3>
          {txHash && onTrace && (
            <button
              onClick={() => {
                if (traceData) {
                    setViewMode("trace");
                } else {
                    onTrace(txHash);
                    setViewMode("trace");
                }
              }}
              disabled={isLoadingTrace}
              className={clsx(
                "text-[10px] flex items-center gap-1 px-2 py-0.5 rounded border transition-colors",
                viewMode === "trace"
                  ? "bg-indigo-900/30 text-indigo-400 border-indigo-900/50"
                  : "bg-slate-900 text-slate-400 border-slate-800 hover:text-white"
              )}
            >
              {isLoadingTrace ? (
                <Loader2 size={10} className="animate-spin" />
              ) : (
                <Activity size={10} />
              )}{" "}
              Trace
            </button>
          )}
        </div>
        <div className="flex bg-slate-800 rounded p-0.5">
          <button
            onClick={() => setViewMode("pretty")}
            className={clsx(
              "px-3 py-1 text-[10px] font-medium rounded-sm transition-all",
              viewMode === "pretty"
                ? "bg-slate-600 text-slate-100 shadow-sm"
                : "text-slate-400 hover:text-slate-300"
            )}
          >
            Pretty
          </button>
          <button
            onClick={() => setViewMode("raw")}
            className={clsx(
              "px-3 py-1 text-[10px] font-medium rounded-sm transition-all",
              viewMode === "raw"
                ? "bg-slate-600 text-slate-100 shadow-sm"
                : "text-slate-400 hover:text-slate-300"
            )}
          >
            Raw
          </button>
        </div>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-auto p-6">
        {viewMode === "pretty" && renderPretty()}

        {viewMode === "raw" && (
          <pre className="text-green-400 bg-slate-900/30 p-4 rounded-lg overflow-auto text-xs font-mono">
            {JSON.stringify(
              data,
              (_, v) => (typeof v === "bigint" ? v.toString() : v),
              2
            )}
          </pre>
        )}

        {viewMode === "trace" && (
            traceData ? (
                <div className="relative h-full flex flex-col">
                    <div className="flex-1 overflow-auto bg-slate-900/50 p-4 rounded border border-slate-800">
                        <Editor
                        value={stripAnsi(traceData)}
                        onValueChange={() => {}} // Read-only
                        highlight={(code) =>
                            highlight(code, languages.clike, "clike")
                        }
                        padding={12}
                        className="font-mono text-[10px] leading-relaxed"
                        style={{
                            fontFamily: "monospace",
                            backgroundColor: "transparent",
                            color: "#e2e8f0",
                            minHeight: "100%",
                        }}
                        textareaClassName="focus:outline-none"
                        readOnly
                        />
                    </div>
                </div>
            ) : (
                <div className="h-full flex items-center justify-center text-slate-500 text-xs">
                    {isLoadingTrace ? "Fetching trace..." : "No trace data available."}
                </div>
            )
        )}
      </div>
    </div>
  );
}
