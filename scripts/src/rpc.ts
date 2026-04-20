import { connect, keyStores, type Near } from "near-api-js";

import {
  CREDENTIALS_DIR,
  EXPECTED_CHAIN_ID,
  FASTNEAR_API_KEY,
  NEAR_NETWORK,
  RPC_AUDIT,
  RPC_SEND,
} from "./config.js";

function authHeaders(): Record<string, string> {
  const h: Record<string, string> = { "Content-Type": "application/json" };
  if (FASTNEAR_API_KEY) {
    h["Authorization"] = `Bearer ${FASTNEAR_API_KEY}`;
  }
  return h;
}

export type JsonRpcError = { code: number; message: string; data?: unknown };

export class RpcError extends Error {
  public readonly code: number;
  public readonly data: unknown;
  constructor(code: number, message: string, data: unknown) {
    super(
      `RPC ${code}: ${message}${data !== undefined ? ` — ${JSON.stringify(data)}` : ""}`,
    );
    this.code = code;
    this.data = data;
    this.name = "RpcError";
  }
}

function isTransientRpcError(err: RpcError): boolean {
  const data = typeof err.data === "string" ? err.data : JSON.stringify(err.data ?? "");
  const payload = `${err.message} ${data}`.toLowerCase();
  if (/overload|timeout|too\s+busy|too\s+many\s+requests/.test(payload)) return true;
  return false;
}

async function rpcCall<T>(url: string, method: string, params: unknown): Promise<T> {
  const body = JSON.stringify({ jsonrpc: "2.0", id: "sequential", method, params });
  const maxAttempts = 8;
  let lastErr: unknown;
  for (let attempt = 1; attempt <= maxAttempts; attempt++) {
    try {
      const res = await fetch(url, { method: "POST", headers: authHeaders(), body });
      if (res.status === 429 || res.status >= 500) {
        lastErr = new Error(`HTTP ${res.status}`);
        if (attempt === maxAttempts) break;
        const backoffMs = Math.min(8000, 500 * 2 ** (attempt - 1));
        await new Promise((r) => setTimeout(r, backoffMs));
        continue;
      }
      const json = (await res.json()) as { result?: T; error?: JsonRpcError };
      if (json.error) {
        const rpcErr = new RpcError(json.error.code, json.error.message, json.error.data);
        if (isTransientRpcError(rpcErr) && attempt < maxAttempts) {
          lastErr = rpcErr;
          const backoffMs = Math.min(8000, 500 * 2 ** (attempt - 1));
          await new Promise((r) => setTimeout(r, backoffMs));
          continue;
        }
        throw rpcErr;
      }
      if (json.result === undefined) {
        throw new Error(`RPC returned no result for ${method}`);
      }
      return json.result;
    } catch (err) {
      lastErr = err;
      if (err instanceof RpcError) throw err;
      if (attempt === maxAttempts) break;
      const backoffMs = Math.min(8000, 500 * 2 ** (attempt - 1));
      await new Promise((r) => setTimeout(r, backoffMs));
    }
  }
  throw lastErr ?? new Error(`RPC ${method} failed after ${maxAttempts} attempts`);
}

export async function sendRpc<T = unknown>(method: string, params: unknown): Promise<T> {
  return rpcCall<T>(RPC_SEND, method, params);
}

export async function auditRpc<T = unknown>(method: string, params: unknown): Promise<T> {
  return rpcCall<T>(RPC_AUDIT, method, params);
}

// Guard against mis-configured RPC accidentally talking to the wrong chain.
// Every broadcasting command MUST call this before signing.
export async function assertChainIdMatches(): Promise<void> {
  const status = await sendRpc<{ chain_id: string }>("status", []);
  if (status.chain_id !== EXPECTED_CHAIN_ID) {
    throw new Error(
      `chain_id mismatch: RPC reports "${status.chain_id}" but NEAR_NETWORK=${NEAR_NETWORK} expects "${EXPECTED_CHAIN_ID}"`,
    );
  }
}

export async function accountExists(accountId: string): Promise<boolean> {
  try {
    await sendRpc("query", {
      request_type: "view_account",
      finality: "final",
      account_id: accountId,
    });
    return true;
  } catch (err) {
    if (err instanceof RpcError) return false;
    throw err;
  }
}

export async function getBlockHeight(): Promise<bigint> {
  const block = await sendRpc<{ header: { height: number } }>("block", { finality: "final" });
  return BigInt(block.header.height);
}

export async function viewCall<T = unknown>(
  accountId: string,
  methodName: string,
  args: Record<string, unknown>,
): Promise<T> {
  const argsBase64 = Buffer.from(JSON.stringify(args)).toString("base64");
  const result = await auditRpc<{ result: number[] }>("query", {
    request_type: "call_function",
    account_id: accountId,
    method_name: methodName,
    args_base64: argsBase64,
    finality: "final",
  });
  const buf = Buffer.from(result.result);
  return JSON.parse(buf.toString("utf8")) as T;
}

export interface ReceiptOutcomeEntry {
  id: string;
  outcome: {
    logs: string[];
    receipt_ids: string[];
    gas_burnt: number;
    status:
      | { SuccessValue: string }
      | { SuccessReceiptId: string }
      | { Failure: unknown }
      | { Unknown: unknown };
  };
}

export interface TxStatusResult {
  status:
    | { SuccessValue: string }
    | { Failure: unknown }
    | { Unknown: unknown };
  transaction: { hash: string; signer_id: string; receiver_id: string };
  transaction_outcome: ReceiptOutcomeEntry;
  receipts_outcome: ReceiptOutcomeEntry[];
}

export async function txStatus(
  txHash: string,
  senderId: string,
  waitUntil: "NONE" | "INCLUDED" | "EXECUTED_OPTIMISTIC" | "EXECUTED" | "FINAL" = "FINAL",
): Promise<TxStatusResult> {
  return auditRpc<TxStatusResult>("EXPERIMENTAL_tx_status", {
    tx_hash: txHash,
    sender_account_id: senderId,
    wait_until: waitUntil,
  });
}

/// Wait for the OUTER receipt of a submit_intent tx to execute, without
/// blocking on the yielded callback's resolution. `EXECUTED_OPTIMISTIC`
/// waits for all receipts including the yield, which stays pending for
/// ~200 blocks (the NEP-519 default wait). By the time the RPC returns,
/// the yield has already timed out and the gate's timeout-arm has
/// removed pending state — so a follow-up `resume_intent` panics with
/// "unknown intent_id". Observed on first fee-mechanism testnet run.
///
/// Strategy: poll `EXPERIMENTAL_tx_status` with `NONE` wait; return as
/// soon as any receipt_outcome has completed (has status). The outer
/// receipt is first in the chain, so the first non-empty
/// `receipts_outcome` entry is the one that contains the
/// `intent_submitted` trace and status. The yielded callback's receipt
/// stays unresolved, which is fine — we no longer depend on it.
export async function waitForOuterReceipt(
  txHash: string,
  senderId: string,
  timeoutMs = 30_000,
): Promise<TxStatusResult> {
  const deadline = Date.now() + timeoutMs;
  let attempt = 0;
  while (Date.now() < deadline) {
    attempt += 1;
    try {
      const status = await txStatus(txHash, senderId, "NONE");
      const outer = status.receipts_outcome?.[0];
      // A receipt's `outcome.status` may be the literal string "Started"
      // (not-yet-executed) or a terminal object
      // ({SuccessValue:...} | {SuccessReceiptId:...} | {Failure:...}).
      // Only the object form means the outer call finished and emitted
      // its logs — which is what we need to extract intent_id.
      if (outer && outer.outcome && typeof outer.outcome.status === "object") {
        return status;
      }
    } catch (err) {
      // Tx not yet propagated / included ("doesn't exist", "unknown
      // transaction") is expected while we're polling faster than
      // chunk-inclusion. Fall through to retry. Real server errors
      // (code != -32000 or different message body) bubble up.
      if (err instanceof RpcError) {
        const payload = `${err.message} ${typeof err.data === "string" ? err.data : JSON.stringify(err.data ?? "")}`.toLowerCase();
        if (!/unknown|doesn't exist|does not exist|not found|pending|included/.test(payload)) {
          throw err;
        }
      } else {
        throw err;
      }
    }
    const backoffMs = Math.min(1500, 250 + attempt * 100);
    await new Promise((r) => setTimeout(r, backoffMs));
  }
  throw new Error(
    `waitForOuterReceipt(${txHash}) timed out after ${timeoutMs}ms without outer-receipt execution`,
  );
}

/// Parse `trace:{...}` JSON lines from a full tx's receipt DAG.
export function extractTraceEvents(status: TxStatusResult): Array<Record<string, unknown>> {
  const events: Array<Record<string, unknown>> = [];
  const allLogs = [
    ...(status.transaction_outcome?.outcome?.logs ?? []),
    ...status.receipts_outcome.flatMap((r) => r.outcome.logs),
  ];
  for (const line of allLogs) {
    if (!line.startsWith("trace:")) continue;
    try {
      events.push(JSON.parse(line.slice("trace:".length)) as Record<string, unknown>);
    } catch {
      // Non-JSON trace lines are just dropped; the gate emits valid JSON.
    }
  }
  return events;
}

let _connected: Promise<Near> | null = null;
export async function connectSender(): Promise<Near> {
  if (_connected) return _connected;
  const keyStore = new keyStores.UnencryptedFileSystemKeyStore(CREDENTIALS_DIR);
  _connected = connect({
    networkId: NEAR_NETWORK,
    keyStore,
    nodeUrl: RPC_SEND,
    headers: FASTNEAR_API_KEY ? { Authorization: `Bearer ${FASTNEAR_API_KEY}` } : {},
  });
  return _connected;
}
