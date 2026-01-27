import { useState, useMemo, useEffect, useCallback, useRef } from "react";
import {
  Play,
  Loader2,
  ChevronDown,
  ChevronUp,
  RotateCcw,
  Zap,
  Link,
} from "lucide-react";
import { clsx } from "clsx";
import type { PublicClient, WalletClient, Address, Hex, TestClient } from "viem";
import {
  formatEther,
  encodeFunctionData,
  decodeFunctionData,
  parseUnits,
  formatUnits,
  isAddress,
} from "viem";

import Editor from "react-simple-code-editor";
import { Cheatcodes } from "./Cheatcodes";
import { highlight, languages } from "prismjs";
import "prismjs/components/prism-json";
import "prismjs/components/prism-clike";

interface RequestTabProps {
  type: "deploy" | "function";
  contractName: string;
  abiItem?: any;
  bytecode?: string;
  contractAddress?: Address | null;
  publicClient: PublicClient;
  walletClient: WalletClient;
  onLog: (msg: string) => void;
  onDeploySuccess?: (address: Address) => void;
  onDeployRequest?: () => void;
  rpcUrl?: string;
  globalMode: "live" | "local";
  ensureLocalClients: () => Promise<{ publicClient: PublicClient; walletClient: WalletClient; testClient: TestClient; rpcUrl: string }>;
  localClients: { publicClient: PublicClient; walletClient: WalletClient; testClient: TestClient; rpcUrl: string } | null;
  onSnapshotCreated?: (entry: {
    snapshotId: string;
    method: string;
    from?: string;
    to?: string;
    value?: string;
  }) => string;
  onSnapshotUpdated?: (id: string, patch: { txHash?: string; blockNumber?: number; status?: "confirmed" | "error" }) => void;
  snapshotsCount?: number;
}

const COMMON_KEYS = [
  "transactionHash",
  "hash",
  "from",
  "to",
  "contractAddress",
  "blockNumber",
  "status",
  "value",
  "gasUsed",
  "gasPrice",
];

type ValueUnit = "wei" | "gwei" | "ether";

const isArrayType = (type: string) => /\[[0-9]*\]$/.test(type);

const parseArrayType = (type: string) => {
  const match = type.match(/^(.*)\[(\d*)\]$/);
  if (!match) return { baseType: type, length: null as number | null };
  const length = match[2] ? Number(match[2]) : null;
  return { baseType: match[1], length: Number.isNaN(length) ? null : length };
};

const coercePrimitive = (val: string) => {
  if (val === "true") return true;
  if (val === "false") return false;
  return val;
};

function UnitDisplay({ value }: { value: bigint }) {
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
      prev === "ether" ? "gwei" : prev === "gwei" ? "wei" : "ether",
    );
  };

  return (
    <span
      onClick={nextUnit}
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

export function RequestTab({
  type,
  contractName,
  abiItem,
  bytecode,
  contractAddress,
  publicClient,
  walletClient,
  onLog,
  onDeploySuccess,
  onDeployRequest,
  rpcUrl,
  globalMode,
  ensureLocalClients,
  localClients,
  onSnapshotCreated,
  onSnapshotUpdated,
  snapshotsCount = 0,
}: RequestTabProps) {
  // Form State
  const [inputs, setInputs] = useState<Record<number, string | string[]>>({});
  const [value, setValue] = useState<string>("");
  const [valueUnit, setValueUnit] = useState<ValueUnit>("ether");
  const [gasLimit, setGasLimit] = useState<string>("");

  // Deploy Mode State
  const [deployMode, setDeployMode] = useState<"create" | "at_address">(
    "create",
  );
  const [existingAddress, setExistingAddress] = useState("");

  // UI State
  const [response, setResponse] = useState<any>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [lastExecutionMode, setLastExecutionMode] = useState<
    "live" | "local" | null
  >(null);
  const lastExecutionHadErrorRef = useRef(false);
  const [lastTxHash, setLastTxHash] = useState<string | null>(null);
  const [lastCallParams, setLastCallParams] = useState<any>(null);
  const [traceHint, setTraceHint] = useState<string | null>(null);
  const [viewMode, setViewMode] = useState<"pretty" | "raw" | "trace">(
    "pretty",
  );
  const [showCheatcodes, setShowCheatcodes] = useState(false);
  const [localTestClient, setLocalTestClient] = useState<TestClient | null>(null);
  const [localPublicClient, setLocalPublicClient] = useState<PublicClient | null>(null);
  const pendingSnapshotIdRef = useRef<string | null>(null);


  useEffect(() => {
    if (globalMode !== "local") {
      setLocalTestClient(null);
      setLocalPublicClient(null);
      return;
    }
    if (!showCheatcodes) return;
    let mounted = true;
    ensureLocalClients()
      .then((clients) => {
        if (!mounted) return;
        setLocalTestClient(clients.testClient);
        setLocalPublicClient(clients.publicClient);
      })
      .catch(() => {});
    return () => {
      mounted = false;
    };
  }, [globalMode, showCheatcodes, ensureLocalClients]);
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [requestViewMode, setRequestViewMode] = useState<"form" | "rpc">(
    "form",
  );

  const [isUnitDropdownOpen, setIsUnitDropdownOpen] = useState(false);
  const unitDropdownRef = useRef<HTMLDivElement>(null);

  // Raw State
  const [rawJsonInput, setRawJsonInput] = useState<string>("");

  // Trace State
  const [traceData, setTraceData] = useState<string | null>(null);
  const [loadingTrace, setLoadingTrace] = useState(false);

  const inputsList =
    type === "deploy" ? abiItem?.inputs || [] : abiItem?.inputs || [];

  const buildArgs = useCallback(() => {
    return inputsList.map((input: any, i: number) => {
      const val = inputs[i];
      if (isArrayType(input.type)) {
        const arr = Array.isArray(val) ? val : [];
        return arr.map((item) => coercePrimitive(item));
      }
      if (Array.isArray(val)) {
        return coercePrimitive(val.join(","));
      }
      return coercePrimitive(val || "");
    });
  }, [inputsList, inputs]);

  const isPayable =
    type === "deploy"
      ? abiItem?.stateMutability === "payable"
      : abiItem?.stateMutability === "payable";

  const isView =
    type === "function" &&
    (abiItem?.stateMutability === "view" ||
      abiItem?.stateMutability === "pure");

  const canTrace =
    !isView &&
    Boolean(
      response?.transactionHash ||
        response?.hash ||
        lastTxHash ||
        lastCallParams ||
        error,
    );

  useEffect(() => {
    const handleClick = (e: MouseEvent) => {
      if (
        unitDropdownRef.current &&
        !unitDropdownRef.current.contains(e.target as Node)
      ) {
        setIsUnitDropdownOpen(false);
      }
    };
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, []);

  const getValueInWei = useCallback(() => {
    if (!value) return undefined;
    try {
      return parseUnits(
        value,
        valueUnit === "wei" ? 0 : valueUnit === "gwei" ? 9 : 18,
      );
    } catch {
      return undefined;
    }
  }, [value, valueUnit]);

  const handleUnitChange = (newUnit: ValueUnit) => {
    if (!value) {
      setValueUnit(newUnit);
      setIsUnitDropdownOpen(false);
      return;
    }
    try {
      const currentWei = getValueInWei();
      if (currentWei !== undefined) {
        const newValue = formatUnits(
          currentWei,
          newUnit === "wei" ? 0 : newUnit === "gwei" ? 9 : 18,
        );
        setValue(newValue);
      }
      setValueUnit(newUnit);
    } catch (e) {
      console.error("Conversion failed", e);
      setValueUnit(newUnit);
    }
    setIsUnitDropdownOpen(false);
  };

  const generateRawJson = useCallback(() => {
    try {
    const args = buildArgs();
      const valBigInt = getValueInWei();
      const gas = gasLimit ? BigInt(gasLimit) : undefined;

      let jsonObj: any = {};

      if (type === "deploy") {
        if (deployMode === "at_address") {
          return ""; // No raw RPC for "At Address"
        }
        jsonObj = {
          method: "eth_sendTransaction",
          params: [
            {
              from: walletClient.account?.address,
              data: bytecode || "0x",
              value: valBigInt ? `0x${valBigInt.toString(16)}` : "0x0",
              gas: gas ? `0x${gas.toString(16)}` : undefined,
            },
          ],
        };
      } else {
        const data = encodeFunctionData({
          abi: [abiItem],
          functionName: abiItem.name,
          args,
        });

        jsonObj = {
          method: isView ? "eth_call" : "eth_sendTransaction",
          params: [
            {
              to: contractAddress,
              from: walletClient.account?.address,
              data,
              value: valBigInt
                ? `0x${valBigInt.toString(16)}`
                : isView
                  ? undefined
                  : "0x0",
              gas: gas ? `0x${gas.toString(16)}` : undefined,
            },
            isView ? "latest" : undefined,
          ].filter(Boolean),
        };
      }
      return JSON.stringify(
        jsonObj,
        (_, v) => (typeof v === "bigint" ? `0x${v.toString(16)}` : v),
        2,
      );
    } catch (e) {
      return "";
    }
  }, [
    inputs,
    getValueInWei,
    gasLimit,
    type,
    abiItem,
    bytecode,
    contractAddress,
    isView,
    walletClient.account,
    deployMode,
  ]);

  useEffect(() => {
    if (requestViewMode === "form") {
      const json = generateRawJson();
      if (json) setRawJsonInput(json);
    }
  }, [generateRawJson, requestViewMode]);

  const handleResetRaw = () => {
    const json = generateRawJson();
    if (json) setRawJsonInput(json);
  };

  const handleViewSwitch = (mode: "form" | "rpc") => {
    if (mode === "form" && requestViewMode === "rpc") {
      try {
        const obj = JSON.parse(rawJsonInput);
        const params = obj.params?.[0] || {};
        if (params.value) {
          const valWei = BigInt(params.value);
          setValue(
            formatUnits(
              valWei,
              valueUnit === "wei" ? 0 : valueUnit === "gwei" ? 9 : 18,
            ),
          );
        }
        if (params.gas) {
          setGasLimit(BigInt(params.gas).toString());
        }
        if (type === "function" && params.data) {
          const decoded = decodeFunctionData({
            abi: [abiItem],
            data: params.data,
          });
          if (decoded.args) {
            const newInputs: Record<number, string | string[]> = {};
            decoded.args.forEach((arg: any, i: number) => {
              if (Array.isArray(arg)) {
                newInputs[i] = arg.map((item) => String(item));
              } else {
                newInputs[i] = String(arg);
              }
            });
            setInputs(newInputs);
          }
        }
      } catch (e) {
        console.error("Failed to sync raw to form", e);
      }
    }
    setRequestViewMode(mode);
  };

  const buildCallParams = useCallback(() => {
    const valBigInt = getValueInWei();
    const gas = gasLimit ? BigInt(gasLimit) : undefined;
    if (type === "deploy") {
      return {
        from: walletClient.account?.address,
        data: bytecode || "0x",
        value: valBigInt ? `0x${valBigInt.toString(16)}` : "0x0",
        gas: gas ? `0x${gas.toString(16)}` : undefined,
      };
    }

    let data = "0x";
    try {
      const args = buildArgs();
      data = encodeFunctionData({
        abi: [abiItem],
        functionName: abiItem?.name,
        args,
      });
    } catch {}

    return {
      from: walletClient.account?.address,
      to: contractAddress || undefined,
      data,
      value: valBigInt ? `0x${valBigInt.toString(16)}` : "0x0",
      gas: gas ? `0x${gas.toString(16)}` : undefined,
    };
  }, [
    type,
    walletClient.account?.address,
    bytecode,
    contractAddress,
    abiItem,
    buildArgs,
    getValueInWei,
    gasLimit,
  ]);

  const handleExecute = async () => {
    const shouldAutoTrace = viewMode === "trace";
    const callParamsSnapshot = (() => {
      if (requestViewMode === "rpc") {
        try {
          const parsed = JSON.parse(rawJsonInput);
          return parsed?.params?.[0] || null;
        } catch {
          return null;
        }
      }
      return buildCallParams();
    })();
    setLoading(true);
    setError(null);
    setResponse(null);
    setTraceData(null);
    setShowAdvanced(false);
    setLastTxHash(null);
    setLastCallParams(null);
    setTraceHint(null);
    setLastExecutionMode(globalMode);
    lastExecutionHadErrorRef.current = false;
    if (callParamsSnapshot) {
      setLastCallParams(callParamsSnapshot);
    }

    try {
      if (type !== "deploy" && !contractAddress) {
        throw new Error("NOT_DEPLOYED");
      }

      // Handle "At Address" logic
      if (type === "deploy" && deployMode === "at_address") {
        if (!isAddress(existingAddress)) throw new Error("Invalid Address");
        onLog(`Attaching ${contractName} at ${existingAddress}...`);
        if (onDeploySuccess) {
          onDeploySuccess(existingAddress);
          setResponse({ status: "Attached", address: existingAddress });
        }
        setLoading(false);
        return;
      }

      const runWithClients = async (clients: {
        publicClient: PublicClient;
        walletClient: WalletClient;
        testClient?: TestClient | null;
        rpcUrl: string;
      }) => {
        let snapshotEntryId: string | null = null;
        const createSnapshotEntry = async (
          info: { method: string; from?: string; to?: string; value?: string },
          markPending = false,
          patch?: { txHash?: string; blockNumber?: number; status?: "confirmed" | "error" },
        ) => {
          if (globalMode !== "local") return null;
          if (!clients.testClient || !onSnapshotCreated) return null;
          const snapshotId = await clients.testClient.snapshot();
          snapshotEntryId = onSnapshotCreated({
            snapshotId,
            method: info.method,
            from: info.from,
            to: info.to,
            value: info.value,
          });
          if (markPending) {
            pendingSnapshotIdRef.current = snapshotEntryId;
          }
          if (snapshotEntryId && onSnapshotUpdated && patch) {
            onSnapshotUpdated(snapshotEntryId, patch);
          }
          return snapshotEntryId;
        };
        if (requestViewMode === "rpc") {
          const req = JSON.parse(rawJsonInput);
          onLog(`Executing Raw ${req.method}...`);

          if (type === "deploy" && req.params[0].data.includes("...")) {
            throw new Error(
              "Raw data is truncated. Reset view to regenerate full data.",
            );
          }

          const callParams = req.params?.[0] || {};
          setLastCallParams(callParams);

          // INTERCEPTION LOGIC: Handle eth_sendTransaction locally
          if (req.method === "eth_sendTransaction") {
             const txParams = req.params[0];
             onLog(`Intercepting eth_sendTransaction for local signing...`);
             
             if (!clients.walletClient.account) {
                 throw new Error("Wallet account not initialized");
             }

             // Prepare params for viem's sendTransaction
             const hash = await clients.walletClient.sendTransaction({
                 to: txParams.to,
                 data: txParams.data,
                 value: txParams.value ? BigInt(txParams.value) : undefined,
                 gas: txParams.gas ? BigInt(txParams.gas) : undefined,
                 account: clients.walletClient.account,
                 chain: clients.walletClient.chain
             } as any);

             onLog(`Tx: ${hash}`);
             setResponse({ transactionHash: hash, status: "pending" });
             setLastTxHash(hash);

             const receipt = await clients.publicClient.waitForTransactionReceipt({
               hash,
             });
             const tx = await clients.publicClient.getTransaction({ hash });
             setResponse({ ...receipt, ...tx });
             
             if (snapshotsCount === 0) {
                 await createSnapshotEntry({
                    method: "Raw Transaction",
                    from: clients.walletClient.account?.address,
                    to: txParams.to,
                    value: txParams.value ? `${formatEther(BigInt(txParams.value))} ETH` : "0 ETH",
                 }, false, {
                    txHash: hash,
                    blockNumber: Number(receipt.blockNumber),
                    status: "confirmed",
                 });
             }
             pendingSnapshotIdRef.current = null;
             onLog(`Confirmed in block ${receipt.blockNumber}`);
             
          } else {
              // ORIGINAL LOGIC for other methods (eth_call, etc.)
              const activeClient = isView ? clients.publicClient : clients.walletClient;
              
              if (req.method !== "eth_call") {
                if (snapshotsCount === 0) {
                  const preBlock = await clients.publicClient.getBlockNumber();
                  await createSnapshotEntry({
                    method: req.method,
                    from: req.params?.[0]?.from,
                    to: req.params?.[0]?.to,
                    value: req.params?.[0]?.value,
                  }, true, { status: "confirmed", blockNumber: Number(preBlock) });
                }
              }

              const res = await (activeClient as any).request(req);
    
              if (req.method === "eth_call") {
                setResponse(res);
                onLog(`Result: ${res}`);
              } else {
                onLog(`Tx: ${res}`);
                setResponse({ transactionHash: res, status: "pending" });
                setLastTxHash(res);
                const receipt = await clients.publicClient.waitForTransactionReceipt({
                  hash: res,
                });
                const tx = await clients.publicClient.getTransaction({ hash: res });
                setResponse({ ...receipt, ...tx });
                await createSnapshotEntry({
                  method: req.method,
                  from: req.params?.[0]?.from,
                  to: req.params?.[0]?.to,
                  value: req.params?.[0]?.value,
                }, false, {
                  txHash: res,
                  blockNumber: Number(receipt.blockNumber),
                  status: "confirmed",
                });
                pendingSnapshotIdRef.current = null;
                onLog(`Confirmed in block ${receipt.blockNumber}`);
              }
          }
        } else {
      const args = buildArgs();

          const valBigInt = getValueInWei();
          const gas = gasLimit ? BigInt(gasLimit) : undefined;

          if (type === "deploy") {
            if (!bytecode) throw new Error("No bytecode");
            onLog(`Deploying ${contractName}...`);
            const deployParams: any = {
              abi: abiItem ? [abiItem] : [],
              bytecode: bytecode as Hex,
              args: args,
              value: valBigInt,
              gas,
            };
            if (clients.walletClient.account) {
              deployParams.account = clients.walletClient.account;
            }
            if (snapshotsCount === 0) {
              const preBlock = await clients.publicClient.getBlockNumber();
              await createSnapshotEntry({
                method: `Deploy ${contractName}`,
                from: clients.walletClient.account?.address,
                to: "0x",
                value: valBigInt ? `${formatEther(valBigInt)} ETH` : "0 ETH",
              }, true, { status: "confirmed", blockNumber: Number(preBlock) });
            }
            const hash = await clients.walletClient.deployContract(deployParams);

            onLog(`Deploy Tx: ${hash}`);
            setLastCallParams({
              from: clients.walletClient.account?.address,
              data: bytecode || "0x",
              value: valBigInt ? `0x${valBigInt.toString(16)}` : "0x0",
              gas: gas ? `0x${gas.toString(16)}` : undefined,
            });
            setResponse({ transactionHash: hash, status: "pending" });
            setLastTxHash(hash);

            const receipt = await clients.publicClient.waitForTransactionReceipt({
              hash,
            });
            const tx = await clients.publicClient.getTransaction({ hash });
            setResponse({ ...receipt, ...tx });
            await createSnapshotEntry({
              method: `Deploy ${contractName}`,
              from: clients.walletClient.account?.address,
              to: receipt.contractAddress || "0x",
              value: valBigInt ? `${formatEther(valBigInt)} ETH` : "0 ETH",
            }, false, {
              txHash: hash,
              blockNumber: Number(receipt.blockNumber),
              status: "confirmed",
            });
            pendingSnapshotIdRef.current = null;

            if (receipt.contractAddress && onDeploySuccess) {
              onDeploySuccess(receipt.contractAddress);
              onLog(`Deployed at ${receipt.contractAddress}`);
            }
          } else {
            if (isView) {
              onLog(`Reading ${abiItem.name}...`);
              const res = await clients.publicClient.readContract({
                address: contractAddress!,
                abi: [abiItem],
                functionName: abiItem.name,
                args: args,
              });
              setResponse(res);
              onLog(`Result: ${res}`);
            } else {
              onLog(`Sending ${abiItem.name}...`);
              const writeParams: any = {
                address: contractAddress!,
                abi: [abiItem],
                functionName: abiItem.name,
                args: args,
                value: valBigInt,
                gas,
              };
              if (clients.walletClient.account) {
                writeParams.account = clients.walletClient.account;
              }
              if (snapshotsCount === 0) {
                const preBlock = await clients.publicClient.getBlockNumber();
                await createSnapshotEntry({
                  method: abiItem.name,
                  from: clients.walletClient.account?.address,
                  to: contractAddress!,
                  value: valBigInt ? `${formatEther(valBigInt)} ETH` : "0 ETH",
                }, true, { status: "confirmed", blockNumber: Number(preBlock) });
              }
              const hash = await clients.walletClient.writeContract(writeParams);
              const data = encodeFunctionData({
                abi: [abiItem],
                functionName: abiItem.name,
                args,
              });
              setLastCallParams({
                from: clients.walletClient.account?.address,
                to: contractAddress!,
                data,
                value: valBigInt ? `0x${valBigInt.toString(16)}` : "0x0",
                gas: gas ? `0x${gas.toString(16)}` : undefined,
              });
              onLog(`Tx: ${hash}`);
              setResponse({ transactionHash: hash, status: "pending" });
              setLastTxHash(hash);

              const receipt = await clients.publicClient.waitForTransactionReceipt({
                hash,
              });
              const tx = await clients.publicClient.getTransaction({ hash });
              setResponse({ ...receipt, ...tx });
              await createSnapshotEntry({
                method: abiItem.name,
                from: clients.walletClient.account?.address,
                to: contractAddress!,
                value: valBigInt ? `${formatEther(valBigInt)} ETH` : "0 ETH",
              }, false, {
                txHash: hash,
                blockNumber: Number(receipt.blockNumber),
                status: "confirmed",
              });
              pendingSnapshotIdRef.current = null;
              onLog(`Confirmed in block ${receipt.blockNumber}`);
            }
          }
        }
      };

      if (globalMode === "local") {
        const clients = await ensureLocalClients();
        await runWithClients(clients);
      } else {
        await runWithClients({ publicClient, walletClient, rpcUrl: rpcUrl || "", testClient: null });
      }
    } catch (e: any) {
      console.error(e);
      const msg = e.message || String(e);
      lastExecutionHadErrorRef.current = true;
      if (pendingSnapshotIdRef.current && onSnapshotUpdated) {
        onSnapshotUpdated(pendingSnapshotIdRef.current, { status: "error" });
        pendingSnapshotIdRef.current = null;
      }
      const match = msg.match(/0x[a-fA-F0-9]{64}/);
      if (match?.[0]) {
        setLastTxHash(match[0]);
      }
      if (!lastCallParams && callParamsSnapshot) {
        setLastCallParams(callParamsSnapshot);
      } else if (!lastCallParams) {
        const callFallback = buildCallParams();
        if (callFallback?.data) setLastCallParams(callFallback);
      }
      if (msg === "NOT_DEPLOYED") {
        setError("NOT_DEPLOYED");
        onLog(`Error: Contract ${contractName} is not deployed.`);
      } else {
        setError(msg);
        onLog(`Error: ${msg}`);
      }
    } finally {
      setLoading(false);
      if (shouldAutoTrace) {
        await fetchTrace(callParamsSnapshot || undefined, lastExecutionHadErrorRef.current);
      }
    }
  };

  const fetchTrace = async (callParamsOverride?: any, forceError?: boolean) => {
    setLoadingTrace(true);
    setViewMode("trace");
    const hadError = forceError ?? Boolean(error);

    const traceByTx = async (rpcUrlOverride: string, txHash: string) => {
      const res = await fetch(
        `http://localhost:3000/trace/${txHash}?rpc_url=${encodeURIComponent(rpcUrlOverride)}`,
      );
      const raw = await res.text();
      try {
        const data = JSON.parse(raw);
        if (data.error) setTraceData(`Error: ${data.error}`);
        else setTraceData(data.stdout || data.stderr || "No trace output.");
      } catch {
        setTraceData(`Error: ${raw || "Empty trace response."}`);
      }
      setTraceHint("Tx hash trace");
    };

    const traceByCall = async (rpcUrlOverride: string, callParams: any) => {
      if (!callParams) {
        setTraceData("Trace unavailable: missing call data.");
        return;
      }
      const res = await fetch("http://localhost:3000/trace/calltree", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          rpcUrl: rpcUrlOverride,
          call: callParams,
          blockTag: "latest",
        }),
      });
      const raw = await res.text();
      try {
        const data = JSON.parse(raw);
        if (data.error) {
          setTraceData(`Error: ${data.error}`);
        } else if (data.stdout) {
          setTraceData(data.stdout);
        } else {
          setTraceData(data.stderr || "No trace output.");
        }
      } catch {
        setTraceData(`Error: ${raw || "Empty trace response."}`);
      }
      setTraceHint("Call trace");
    };

    try {
      let effectiveCallParams = callParamsOverride || lastCallParams;
      if (!effectiveCallParams && requestViewMode === "rpc") {
        try {
          const parsed = JSON.parse(rawJsonInput);
          const params = parsed?.params?.[0];
          if (params) {
            effectiveCallParams = params;
            setLastCallParams(params);
          }
        } catch {}
      }
      if (!effectiveCallParams) {
        const fallback = buildCallParams();
        if (fallback?.data) {
          effectiveCallParams = fallback;
          setLastCallParams(fallback);
        }
      }
      const traceMode = lastExecutionMode ?? globalMode;
      if (traceMode === "local") {
        const clients = localClients ?? (await ensureLocalClients());
        const forkRpc = clients.rpcUrl;
        if (!hadError) {
          const txHash = response?.transactionHash || response?.hash || lastTxHash;
          if (!txHash) {
            setTraceData("Trace unavailable: missing transaction hash.");
            return;
          }
          await traceByTx(forkRpc, txHash);
        } else {
          await traceByCall(forkRpc, effectiveCallParams);
        }
      } else if (rpcUrl) {
        if (!hadError) {
          const txHash = response?.transactionHash || response?.hash || lastTxHash;
          if (!txHash) {
            setTraceData("Trace unavailable: missing transaction hash.");
            return;
          }
          await traceByTx(rpcUrl, txHash);
        } else {
          await traceByCall(rpcUrl, effectiveCallParams);
        }
      }
    } catch (e: any) {
      setTraceData(`Failed to fetch trace: ${e.message}`);
    } finally {
      setLoadingTrace(false);
    }
  };

  useEffect(() => {
    if (error && viewMode !== "trace") {
      setViewMode("raw");
    }
  }, [error, viewMode]);

  const renderPrettyResponse = () => {
    if (response === null || response === undefined) return null;

    if (typeof response !== "object" || response === null) {
      return (
        <div className="text-indigo-300 text-sm p-2 font-semibold">
          {String(response)}
        </div>
      );
    }

    const keys = Object.keys(response);
    const commonKeys = keys.filter((k) => COMMON_KEYS.includes(k));
    const advancedKeys = keys.filter((k) => !COMMON_KEYS.includes(k));

    const renderRow = (key: string, val: any) => {
      if (val === null || val === undefined) return null;

      let content = (
        <span className="text-indigo-300 break-all">{String(val)}</span>
      );

      if (
        key === "value" ||
        key === "gasPrice" ||
        key === "effectiveGasPrice"
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
          <span className="text-slate-500 min-w-[120px] font-medium">
            {key}:
          </span>
          {content}
        </div>
      );
    };

    return (
      <div className="space-y-1">
        <div className="space-y-0.5">
          {commonKeys.map((k) => renderRow(k, response[k]))}
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
                {advancedKeys.map((k) => renderRow(k, response[k]))}
              </div>
            )}
          </div>
        )}
      </div>
    );
  };

  const stripAnsi = (str: string) => str.replace(/\x1B\[[0-9;]*[a-zA-Z]/g, "");

  // Line Numbers Calculation
  const lineNumbers = useMemo(() => {
    const lines = rawJsonInput.split("\n").length;
    return Array.from({ length: lines }, (_, i) => i + 1);
  }, [rawJsonInput]);

  return (
    <div className="flex flex-row h-full w-full">
      {/* Request Section */}
      <div className="w-1/2 border-r border-slate-800 flex flex-col min-h-0">
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-2 bg-slate-900/50 border-b border-slate-800 h-[45px]">
          <div className="flex items-center gap-2">
            <h3 className="text-xs font-bold text-slate-500 uppercase tracking-wider">
              Parameters
            </h3>
            {requestViewMode === "rpc" && deployMode === "create" && (
              <button
                onClick={handleResetRaw}
                className="text-slate-500 hover:text-indigo-400 p-1 rounded transition-colors"
                title="Reset to Form Data"
              >
                <RotateCcw size={12} />
              </button>
            )}
          </div>
          <div className="flex items-center gap-2">
            <span className="text-[10px] uppercase text-slate-500 font-semibold">Mode</span>
            <span
              className={clsx(
                "text-[10px] uppercase font-bold px-2 py-1 rounded",
                globalMode === "local"
                  ? "bg-indigo-600/30 text-indigo-200"
                  : "bg-slate-800 text-slate-400",
              )}
            >
              {globalMode}
            </span>
            {type === "deploy" && (
              <div className="flex bg-slate-800 rounded p-0.5 mr-2">
                <button
                  onClick={() => setDeployMode("create")}
                  className={clsx(
                    "px-3 py-1 text-[10px] font-medium rounded-sm transition-all",
                    deployMode === "create"
                      ? "bg-slate-600 text-slate-100 shadow-sm"
                      : "text-slate-400 hover:text-slate-300",
                  )}
                >
                  Create
                </button>
                <button
                  onClick={() => setDeployMode("at_address")}
                  className={clsx(
                    "px-3 py-1 text-[10px] font-medium rounded-sm transition-all",
                    deployMode === "at_address"
                      ? "bg-slate-600 text-slate-100 shadow-sm"
                      : "text-slate-400 hover:text-slate-300",
                  )}
                >
                  At Address
                </button>
              </div>
            )}
            {deployMode === "create" && (
              <div className="flex bg-slate-800 rounded p-0.5">
                <button
                  onClick={() => handleViewSwitch("form")}
                  className={clsx(
                    "px-3 py-1 text-[10px] font-medium rounded-sm transition-all",
                    requestViewMode === "form"
                      ? "bg-slate-600 text-slate-100 shadow-sm"
                      : "text-slate-400 hover:text-slate-300",
                  )}
                >
                  Form
                </button>
                <button
                  onClick={() => handleViewSwitch("rpc")}
                  className={clsx(
                    "px-3 py-1 text-[10px] font-medium rounded-sm transition-all",
                    requestViewMode === "rpc"
                      ? "bg-slate-600 text-slate-100 shadow-sm"
                      : "text-slate-400 hover:text-slate-300",
                  )}
                >
                  Raw
                </button>
              </div>
            )}
          </div>
        </div>

        <div className="flex-1 overflow-y-auto p-6 flex flex-col">
          {deployMode === "at_address" ? (
            <div className="max-w-xl mx-auto space-y-6 w-full pt-10">
              <div className="flex flex-col gap-1.5">
                <label className="text-xs font-medium text-slate-400">
                  Contract Address
                </label>
                <input
                  className="bg-transparent border-b border-slate-700 py-2 px-1 text-sm text-slate-200 focus:border-indigo-500 outline-none transition-colors placeholder:text-slate-700"
                  placeholder="0x..."
                  value={existingAddress}
                  onChange={(e) => setExistingAddress(e.target.value)}
                />
              </div>
              <div className="pt-8">
                <button
                  onClick={() => handleExecute()}
                  disabled={!isAddress(existingAddress)}
                  className="px-6 py-2.5 rounded text-sm font-semibold flex items-center justify-center gap-2 w-full transition-all bg-indigo-600 hover:bg-indigo-500 text-white shadow-lg shadow-indigo-900/20 active:translate-y-0.5 disabled:opacity-50 disabled:cursor-not-allowed"
                >
                  <Link size={16} /> Attach
                </button>
              </div>
            </div>
          ) : requestViewMode === "form" ? (
            <div className="max-w-xl mx-auto space-y-6 w-full">
              <div className="space-y-4">
                {inputsList.length > 0 ? (
                  <div className="grid grid-cols-1 gap-5">
                    {inputsList.map((input: any, i: number) => {
                      const isArray = isArrayType(input.type);
                      const { baseType, length } = parseArrayType(input.type);
                      const rawVal = inputs[i];
                      const arrayValues = Array.isArray(rawVal) ? rawVal : [];
                      const displayValues =
                        length !== null
                          ? Array.from({ length }, (_, idx) => arrayValues[idx] ?? "")
                          : arrayValues.length > 0
                          ? arrayValues
                          : [""];
                      return (
                        <div key={i} className="flex flex-col gap-1.5">
                          <label className="text-xs font-medium text-slate-400">
                            {input.name || `param_${i}`}{" "}
                            <span className="text-slate-600 ml-1">
                              ({input.type})
                            </span>
                          </label>
                          {isArray ? (
                            <div className="space-y-2">
                              {displayValues.map((val, idx) => (
                                <div key={idx} className="flex items-center gap-2">
                                  <input
                                    className="flex-1 bg-transparent border-b border-slate-700 py-2 px-1 text-sm text-slate-200 focus:border-indigo-500 outline-none transition-colors placeholder:text-slate-700"
                                    placeholder={`Item ${idx + 1} (${baseType})`}
                                    value={val}
                                    onChange={(e) => {
                                      const next = [...displayValues];
                                      next[idx] = e.target.value;
                                      setInputs((p) => ({ ...p, [i]: next }));
                                    }}
                                  />
                                  {length === null && (
                                    <button
                                      onClick={() => {
                                        const next = displayValues.filter((_, index) => index !== idx);
                                        setInputs((p) => ({ ...p, [i]: next.length ? next : [""] }));
                                      }}
                                      className="text-[10px] uppercase font-bold text-slate-500 hover:text-red-400"
                                      title="Remove item"
                                    >
                                      Remove
                                    </button>
                                  )}
                                </div>
                              ))}
                              {length === null && (
                                <button
                                  onClick={() => setInputs((p) => ({ ...p, [i]: [...displayValues, ""] }))}
                                  className="text-[10px] uppercase font-bold text-slate-400 hover:text-indigo-300"
                                >
                                  Add item
                                </button>
                              )}
                            </div>
                          ) : (
                            <input
                              className="bg-transparent border-b border-slate-700 py-2 px-1 text-sm text-slate-200 focus:border-indigo-500 outline-none transition-colors placeholder:text-slate-700"
                              placeholder={`Enter ${input.type}`}
                              value={typeof inputs[i] === "string" ? inputs[i] : ""}
                              onChange={(e) =>
                                setInputs((p) => ({ ...p, [i]: e.target.value }))
                              }
                            />
                          )}
                        </div>
                      );
                    })}
                  </div>
                ) : (
                  <div className="text-slate-600 italic text-sm py-4">
                    No parameters required.
                  </div>
                )}

                {/* Options */}
                <div className="pt-6 mt-2 grid grid-cols-2 gap-6">
                  <div className="flex flex-col gap-1.5">
                    <label className="text-xs font-medium text-slate-400">
                      Value
                    </label>
                    <div className="flex gap-2 relative" ref={unitDropdownRef}>
                      <input
                        className="flex-1 bg-transparent border-b border-slate-700 py-2 px-1 text-sm text-slate-200 focus:border-indigo-500 outline-none disabled:opacity-30 disabled:cursor-not-allowed placeholder:text-slate-700"
                        placeholder="0.0"
                        type="number"
                        value={value}
                        onChange={(e) => setValue(e.target.value)}
                        disabled={!isPayable}
                      />
                      <button
                        onClick={() =>
                          isPayable &&
                          setIsUnitDropdownOpen(!isUnitDropdownOpen)
                        }
                        disabled={!isPayable}
                        className="bg-slate-900 border-b border-slate-700 text-xs text-slate-400 focus:outline-none focus:text-slate-200 disabled:opacity-30 cursor-pointer flex items-center gap-1 px-2 hover:bg-slate-800 transition-colors"
                      >
                        {valueUnit.toUpperCase()} <ChevronDown size={10} />
                      </button>

                      {isUnitDropdownOpen && (
                        <div className="absolute top-full right-0 mt-1 bg-slate-900 border border-slate-700 rounded shadow-lg z-50 w-20 overflow-hidden">
                          {["ether", "gwei", "wei"].map((unit) => (
                            <div
                              key={unit}
                              onClick={() =>
                                handleUnitChange(unit as ValueUnit)
                              }
                              className={clsx(
                                "px-3 py-1.5 text-xs cursor-pointer hover:bg-slate-800 transition-colors uppercase",
                                valueUnit === unit
                                  ? "text-indigo-400 font-bold bg-slate-800/50"
                                  : "text-slate-400",
                              )}
                            >
                              {unit === "ether" ? "ETH" : unit}
                            </div>
                          ))}
                        </div>
                      )}
                    </div>
                  </div>
                  <div className="flex flex-col gap-1.5">
                    <label className="text-xs font-medium text-slate-400">
                      Gas Limit
                    </label>
                    <input
                      className="bg-transparent border-b border-slate-700 py-2 px-1 text-sm text-slate-200 focus:border-indigo-500 outline-none placeholder:text-slate-700"
                      placeholder="Auto"
                      type="number"
                      value={gasLimit}
                      onChange={(e) => setGasLimit(e.target.value)}
                    />
                  </div>
                </div>

              </div>
              {type === "function" && globalMode === "local" && (
                <div className="pt-6 border-t border-slate-800/60">
                  <button
                    onClick={() => setShowCheatcodes((prev) => !prev)}
                    className="text-[10px] uppercase font-bold text-slate-400 hover:text-indigo-300 flex items-center gap-2"
                  >
                    {showCheatcodes ? "Hide" : "Show"} Cheatcodes
                    <ChevronDown
                      size={12}
                      className={clsx(
                        "transition-transform",
                        showCheatcodes && "rotate-180",
                      )}
                    />
                  </button>
                  {showCheatcodes && (
                    <div className="mt-4 border border-slate-800 rounded-lg overflow-hidden">
                      <Cheatcodes
                        testClient={localTestClient ?? localClients?.testClient}
                        publicClient={localPublicClient ?? localClients?.publicClient ?? publicClient}
                        onLog={onLog}
                        enabled={globalMode === "local"}
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
                  {lineNumbers.map((i) => (
                    <div key={i}>{i}</div>
                  ))}
                </div>
                <div className="flex-1 relative">
                  <Editor
                    value={rawJsonInput}
                    onValueChange={setRawJsonInput}
                    highlight={(code) =>
                      highlight(code, languages.json, "json")
                    }
                    padding={12}
                    className="font-mono text-[10px] leading-5"
                    style={{
                      backgroundColor: "transparent",
                      color: "#e2e8f0", // slate-200
                      minHeight: "100%",
                    }}
                    textareaClassName="focus:outline-none"
                  />
                </div>
              </div>
              <button
                onClick={handleResetRaw}
                className="absolute top-2 right-2 bg-slate-800 hover:bg-slate-700 text-slate-400 hover:text-white p-1.5 rounded shadow transition-all z-10"
                title="Reset to Form Data"
              >
                <RotateCcw size={14} />
              </button>
            </div>
          )}

          <div className="h-6" />
        </div>
        {/* Common Action Button Area */}
        {deployMode === "create" && (
          <div className="p-6 border-t border-slate-800 bg-slate-950">
            <button
              onClick={() => handleExecute()}
              disabled={loading || (type !== "deploy" && !contractAddress)}
              className={clsx(
                "px-6 py-2.5 rounded text-sm font-semibold flex items-center justify-center gap-2 w-full transition-all",
                loading
                  ? "bg-slate-800 text-slate-500 cursor-wait"
                  : "bg-indigo-600 hover:bg-indigo-500 text-white shadow-lg shadow-indigo-900/20 active:translate-y-0.5",
              )}
            >
              {loading ? (
                <Loader2 className="animate-spin" size={16} />
              ) : (
                <Play size={16} fill="currentColor" />
              )}
              {loading
                ? "Executing..."
                : type === "function" && isView
                  ? "Call"
                  : "Send Transaction"}
            </button>
          </div>
        )}
      </div>

      {/* Response Section */}
      <div className="w-1/2 bg-slate-950 flex flex-col min-h-0">
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-2 bg-slate-900/50 border-b border-slate-800 h-[45px]">
          <div className="flex items-center gap-4">
            <h3 className="text-xs font-bold text-slate-500 uppercase tracking-wider">
              Response
            </h3>
          </div>
          {!error ? (
            <div className="flex bg-slate-800 rounded p-0.5">
              <button
                onClick={() => setViewMode("pretty")}
                className={clsx(
                  "px-3 py-1 text-[10px] font-medium rounded-sm transition-all",
                  viewMode === "pretty"
                    ? "bg-slate-600 text-slate-100 shadow-sm"
                    : "text-slate-400 hover:text-slate-300",
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
                    : "text-slate-400 hover:text-slate-300",
                )}
              >
                Raw
              </button>
              <button
                onClick={() => {
                  setViewMode("trace");
                  fetchTrace();
                }}
                disabled={loadingTrace || !canTrace}
                className={clsx(
                  "px-3 py-1 text-[10px] font-medium rounded-sm transition-all",
                  viewMode === "trace"
                    ? "bg-slate-600 text-slate-100 shadow-sm"
                    : "text-slate-400 hover:text-slate-300",
                  (loadingTrace || !canTrace) && "opacity-50 cursor-not-allowed",
                )}
              >
                {loadingTrace ? "Tracing..." : "Trace"}
              </button>
            </div>
          ) : (
            <div className="flex bg-slate-800 rounded p-0.5">
              <button
                disabled
                className="px-3 py-1 text-[10px] font-medium rounded-sm transition-all text-slate-500 opacity-60 cursor-not-allowed"
              >
                Pretty
              </button>
              <button
                onClick={() => setViewMode("raw")}
                className={clsx(
                  "px-3 py-1 text-[10px] font-medium rounded-sm transition-all",
                  viewMode === "raw"
                    ? "bg-slate-600 text-slate-100 shadow-sm"
                    : "text-slate-400 hover:text-slate-300",
                )}
              >
                Raw
              </button>
              <button
                onClick={() => {
                  setViewMode("trace");
                  fetchTrace();
                }}
                disabled={loadingTrace || !canTrace}
                className={clsx(
                  "px-3 py-1 text-[10px] font-medium rounded-sm transition-all",
                  viewMode === "trace"
                    ? "bg-slate-600 text-slate-100 shadow-sm"
                    : "text-slate-400 hover:text-slate-300",
                  (loadingTrace || !canTrace) && "opacity-50 cursor-not-allowed",
                )}
              >
                {loadingTrace ? "Tracing..." : "Trace"}
              </button>
            </div>
          )}
        </div>

        <div className="flex-1 overflow-auto p-6 text-xs font-mono">
          {viewMode === "pretty" && error === "NOT_DEPLOYED" ? (
            <div className="flex flex-col items-center justify-center h-full space-y-4">
              <div className="text-red-400 bg-red-900/10 border border-red-900/30 px-4 py-2 rounded-md font-bold">
                Contract Not Deployed
              </div>
              <button
                onClick={onDeployRequest}
                className="px-4 py-2 bg-indigo-600 hover:bg-indigo-500 text-white rounded flex items-center gap-2 transition-colors"
              >
                <Zap size={14} fill="currentColor" /> Deploy Contract
              </button>
            </div>
          ) : viewMode === "pretty" && error ? (
            <div className="text-red-400 bg-slate-900/30 border border-red-900/30 p-4 rounded-md font-mono text-xs whitespace-pre-wrap">
              {error}
            </div>
          ) : null}

          {response !== null &&
            response !== undefined &&
            viewMode === "pretty" &&
            renderPrettyResponse()}

          {viewMode === "raw" && (response !== null && response !== undefined) && (
            <pre className="text-green-400 bg-slate-900/30 p-4 rounded-lg overflow-auto">
              {JSON.stringify(
                response,
                (_, v) => (typeof v === "bigint" ? v.toString() : v),
                2,
              )}
            </pre>
          )}

          {viewMode === "raw" && (response === null || response === undefined) && error && (
            <pre className="text-red-400 bg-slate-900/30 p-4 rounded-lg overflow-auto">
              {error}
            </pre>
          )}

          {traceData && viewMode === "trace" && (
            <div className="relative h-full flex flex-col">
              {traceHint && (
                <div className="text-[10px] uppercase font-bold text-slate-500 mb-2">
                  {traceHint}
                </div>
              )}
              <div className="flex-1 overflow-auto bg-slate-900/50 p-4 rounded border border-slate-800">
                <Editor
                  value={stripAnsi(traceData)}
                  onValueChange={() => {}} // Read-only
                  highlight={(code) =>
                    highlight(code, languages.clike, "clike")
                  } // Use clike for better generic highlighting
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
          )}

          {!error && response === null && viewMode !== "raw" && (
            <div className="h-full flex flex-col items-center justify-center text-slate-700 gap-2">
              <div className="w-12 h-12 rounded-full bg-slate-900 flex items-center justify-center">
                <div className="w-2 h-2 bg-slate-800 rounded-full animate-pulse" />
              </div>
              <p>Ready to send request</p>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
