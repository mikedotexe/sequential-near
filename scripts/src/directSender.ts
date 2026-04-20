import { transactions as txns, type Account, type Near } from "near-api-js";
import { baseDecode } from "@near-js/utils";

import { sendRpc } from "./rpc.js";

// A fire-and-forget tx helper that bypasses near-api-js's default
// FINAL-await behavior on account.functionCall(). We need this for
// submit_intent and resume calls: their FINAL DAG includes the
// yielded callback receipt (executed up to 202 blocks later), which
// would otherwise block the runner for ~3.5 minutes per submit.
//
// Ports the DirectSender pattern from the research prototype
// (near-sequencer-demo/scripts/src/tx.ts). Nonce managed locally
// (fetched once, incremented per call) to avoid the InvalidNonce
// retry storm seen when FastNEAR's load balancer returns an access
// key view that lags the chain head.
export interface DirectSender {
  accountId: string;
  broadcastFunctionCall: (
    receiverId: string,
    methodName: string,
    args: Record<string, unknown>,
    gasTgas: number,
    depositYocto: bigint,
  ) => Promise<string>;
}

export async function makeDirectSender(near: Near, accountId: string): Promise<DirectSender> {
  const account: Account = await near.account(accountId);
  const connection = account.connection;
  const provider = connection.provider;
  const signer = connection.signer;

  const publicKey = await signer.getPublicKey(accountId, connection.networkId);
  if (!publicKey) {
    throw new Error(`no public key for ${accountId} on ${connection.networkId}`);
  }
  const pkString = publicKey.toString();

  const accessKeyInfo = (await provider.query({
    request_type: "view_access_key",
    finality: "final",
    account_id: accountId,
    public_key: pkString,
  })) as unknown as { nonce: number | string | bigint };
  let nextNonce = BigInt(accessKeyInfo.nonce) + 1n;

  async function freshBlockHash(): Promise<Uint8Array> {
    const block = await provider.block({ finality: "final" });
    return baseDecode(block.header.hash);
  }

  async function broadcastFunctionCall(
    receiverId: string,
    methodName: string,
    args: Record<string, unknown>,
    gasTgas: number,
    depositYocto: bigint,
  ): Promise<string> {
    const nonce = nextNonce;
    nextNonce += 1n;
    const blockHash = await freshBlockHash();
    const gas = BigInt(gasTgas) * 1_000_000_000_000n;
    const action = txns.functionCall(
      methodName,
      new Uint8Array(Buffer.from(JSON.stringify(args))),
      gas,
      depositYocto,
    );
    const [, signedTx] = await txns.signTransaction(
      receiverId,
      nonce,
      [action],
      blockHash,
      signer,
      accountId,
      connection.networkId,
    );
    const txBytes = signedTx.encode();
    const txBase64 = Buffer.from(txBytes).toString("base64");
    return sendRpc<string>("broadcast_tx_async", [txBase64]);
  }

  return { accountId, broadcastFunctionCall };
}
