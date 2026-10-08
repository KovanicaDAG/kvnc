export interface BlockNumberResponse {
  result: number;
}

export interface BlockResponse {
  hash: string;
  round: number;
  parent_hashes: string[];
  transactions: Array<{ hash: string; raw: string } | string>;
}

export interface GetBlockByHashResponse {
  result: BlockResponse | null;
}

export interface BalanceResponse {
  result: number; // atoms
}

export interface ValidatorInfo {
  address: string;
  stake: number;
  commission_bps: number;
  active: boolean;
  payout_address: string;
}

export interface ValidatorsResponse {
  result: ValidatorInfo[];
}

export interface SendRawTxResponse {
  result: string; // tx hash
}

export interface PendingTxResponse {
  result: string[];
}

export interface EstimateFeeResponse {
  result: number; // atoms/byte
}

interface RpcRequest {
  jsonrpc: "2.0";
  method: string;
  params?: unknown[];
  id?: number | string;
}

export class KvncRpcClient {
  constructor(private url: string = "http://127.0.0.1:8080") {}

  private async call<T>(method: string, params?: unknown[]): Promise<T> {
    const req: RpcRequest = { jsonrpc: "2.0", method, params };
    const res = await fetch(this.url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(req),
    });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const json = await res.json();
    if (json.error) throw new Error(json.error.message || "RPC error");
    return json.result as T;
  }

  blockNumber(): Promise<number> {
    return this.call<number>("kvnc_blockNumber");
  }

  getBlockByHash(hash: string, fullTransactions = false): Promise<BlockResponse | null> {
    return this.call<BlockResponse | null>("kvnc_getBlockByHash", [hash, fullTransactions]);
  }

  getBalance(address: string): Promise<number> {
    return this.call<number>("kvnc_getBalance", [address]);
  }

  getValidators(): Promise<ValidatorInfo[]> {
    return this.call<ValidatorInfo[]>("kvnc_getValidators");
  }

  sendRawTransaction(rawHex: string): Promise<string> {
    return this.call<string>("kvnc_sendRawTransaction", [rawHex]);
  }

  getPendingTransactions(): Promise<string[]> {
    return this.call<string[]>("kvnc_getPendingTransactions");
  }

  estimateFee(): Promise<number> {
    return this.call<number>("kvnc_estimateFee");
  }
}
