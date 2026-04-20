// One-shot: storage-deposit gate.<master> and alice.<master> on the
// Shitzu NEP-141 contract so the gate can dispatch ft_transfer calls.
// NEP-145 `storage_deposit(account_id?, registration_only?)` costs ~
// 0.00125 NEAR per account (one-time). Both accounts need to be
// registered: gate (as the sender — predecessor of the dispatched
// transfer receipt) and the receiver (alice.<master>).
//
// After this runs successfully, fund the gate with some SHITZU via any
// ft_transfer_call / ft_transfer from a SHITZU holder account, then
// exercise:
//
//   NEAR_NETWORK=mainnet MASTER_ACCOUNT_ID=<master> \
//     npx tsx scripts/src/index.ts submit --variant claim --target shitzu
//
// This script intentionally does NOT transfer SHITZU itself — token
// acquisition is out of scope for the gate workflow.

import { ACCOUNTS, MASTER_ACCOUNT_ID, SHITZU_TOKEN } from "../src/config.js";
import { connectSender, viewCall } from "../src/rpc.js";
import { makeDirectSender } from "../src/directSender.js";

const STORAGE_DEPOSIT_YOCTO = 1_250_000_000_000_000_000_000n; // 0.00125 NEAR
const GAS_STORAGE_DEPOSIT_TGAS = 30;

async function storageBalance(accountId: string): Promise<string | null> {
  try {
    const r = await viewCall<{ total: string; available: string } | null>(
      SHITZU_TOKEN,
      "storage_balance_of",
      { account_id: accountId },
    );
    return r === null ? null : r.total;
  } catch (err) {
    console.warn(`storage_balance_of(${accountId}) failed:`, err);
    return null;
  }
}

async function main(): Promise<void> {
  const registrants = [ACCOUNTS.gate, ACCOUNTS.alice];
  console.log(`[shitzu-register] token=${SHITZU_TOKEN} payer=${MASTER_ACCOUNT_ID}`);

  const near = await connectSender();
  const sender = await makeDirectSender(near, MASTER_ACCOUNT_ID);

  for (const acc of registrants) {
    const bal = await storageBalance(acc);
    if (bal !== null) {
      console.log(`  ${acc}: already registered (total=${bal})`);
      continue;
    }
    console.log(`  ${acc}: not registered — firing storage_deposit…`);
    const tx = await sender.broadcastFunctionCall(
      SHITZU_TOKEN,
      "storage_deposit",
      { account_id: acc, registration_only: true },
      GAS_STORAGE_DEPOSIT_TGAS,
      STORAGE_DEPOSIT_YOCTO,
    );
    console.log(`    tx=${tx}`);
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
