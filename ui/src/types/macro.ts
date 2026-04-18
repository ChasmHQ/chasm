export interface MacroStep {
  id: string;
  name: string;
  enabled: boolean;
  // Form mode fields
  contractInstanceId: string;
  functionName: string;
  params: Record<number, string | string[]>;
  // Raw mode fields
  inputMode?: 'form' | 'raw';
  rawTo?: string;
  rawData?: string;
  rawMethod?: 'send' | 'call';
  // Shared
  gasOverride?: string;
  valueWei?: string;
  valueUnit?: 'ether' | 'gwei' | 'wei';
  delayMs?: number;
}

export interface Macro {
  id: string;
  name: string;
  steps: MacroStep[];
  createdAt: number;
  snapshotBeforeRun?: boolean;
  revertOnFailure?: boolean;
}

export interface MacroStepResult {
  stepId: string;
  status: 'pending' | 'running' | 'success' | 'failed' | 'skipped';
  txHash?: string;
  returnValue?: unknown;
  error?: string;
  gasUsed?: string;
  duration?: number;
  blockNumber?: number;
}

export type MacroRunState = {
  isRunning: boolean;
  results: MacroStepResult[];
  snapshotId?: string;
};
