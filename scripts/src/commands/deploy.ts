import { readFileSync } from "node:fs";
import { join } from "node:path";

import {
  ACCOUNTS,
  CONTRACT_INITIAL_BALANCE_NEAR,
  FT_SHIM_TOTAL_SUPPLY,
  GAS_SUBMIT_TGAS,
  MASTER_ACCOUNT_ID,
  NEAR_NETWORK,
  REPO_ROOT,
  USER_INITIAL_BALANCE_NEAR,
  WASM_PATHS,
} from "../config.js";
import {
  assertMasterCredentialPresent,
  ensureSubAccount,
} from "../accounts.js";
import { accountExists, assertChainIdMatches, connectSender } from "../rpc.js";

function wasmBytes(rel: string): Buffer {
  return readFileSync(join(REPO_ROOT, rel));
}

function mainnetConfirmationGate(): void {
  if (NEAR_NETWORK !== "mainnet") return;
  // 3-second soft gate per the main repo's pattern. Gives the user a
  // moment to ctrl-C if they accidentally targeted mainnet.
  const args = process.argv.slice(2);
  if (args.includes("--i-know-this-is-mainnet")) return;
  throw new Error(
    `refusing to deploy to mainnet without --i-know-this-is-mainnet flag`,
  );
}

export async function cmdDeploy(): Promise<void> {
  mainnetConfirmationGate();
  assertMasterCredentialPresent();
  await assertChainIdMatches();

  console.log(`[deploy] network=${NEAR_NETWORK} master=${MASTER_ACCOUNT_ID}`);

  // 1) Create non-contract accounts (user + relayer + approver).
  for (const key of ["alice", "relayer", "approver"] as const) {
    const account = ACCOUNTS[key];
    const res = await ensureSubAccount(account, USER_INITIAL_BALANCE_NEAR);
    console.log(`[deploy] ${key}=${account} ${res}`);
  }

  // 2) Create + deploy + init contract accounts in order:
  //    gate first (its account_id is needed for ft-shim owner).
  await ensureContract(
    "gate",
    ACCOUNTS.gate,
    CONTRACT_INITIAL_BALANCE_NEAR,
    WASM_PATHS.gate,
    {
      owner_id: MASTER_ACCOUNT_ID,
      approver_id: ACCOUNTS.approver,
    },
  );

  await ensureContract(
    "register",
    ACCOUNTS.register,
    CONTRACT_INITIAL_BALANCE_NEAR,
    WASM_PATHS.register,
    null, // register has no init; default state on first call
  );

  await ensureContract(
    "ftShim",
    ACCOUNTS.ftShim,
    CONTRACT_INITIAL_BALANCE_NEAR,
    WASM_PATHS.ftShim,
    {
      owner_id: ACCOUNTS.gate,
      total_supply: FT_SHIM_TOTAL_SUPPLY,
    },
  );

  // 3) Whitelist the relayer on the gate.
  const near = await connectSender();
  const masterAccount = await near.account(MASTER_ACCOUNT_ID);
  await masterAccount.functionCall({
    contractId: ACCOUNTS.gate,
    methodName: "add_relayer",
    args: { account_id: ACCOUNTS.relayer },
    gas: BigInt(GAS_SUBMIT_TGAS) * 1_000_000_000_000n,
  });
  console.log(`[deploy] gate.add_relayer(${ACCOUNTS.relayer}) ok`);

  console.log(`[deploy] all accounts + contracts ready on ${NEAR_NETWORK}`);
}

async function ensureContract(
  role: string,
  accountId: string,
  initialBalanceNear: string,
  wasmRel: string,
  initArgs: Record<string, unknown> | null,
): Promise<void> {
  const created = await ensureSubAccount(accountId, initialBalanceNear);
  const near = await connectSender();
  const account = await near.account(accountId);

  const code = wasmBytes(wasmRel);
  await account.deployContract(code);
  console.log(`[deploy] ${role}=${accountId} ${created} wasm=${wasmRel}`);

  if (initArgs) {
    // Idempotent init: if already-initialized state exists, skip. Simplest
    // check — try a benign view that the init method sets up; if it errors,
    // call init. For v0.1 we just always call init on freshly-created
    // accounts and rely on the "account exists" path for re-runs.
    if (created === "created") {
      await account.functionCall({
        contractId: accountId,
        methodName: "new",
        args: initArgs,
        gas: BigInt(GAS_SUBMIT_TGAS) * 1_000_000_000_000n,
      });
      console.log(`[deploy] ${role}.new(${JSON.stringify(initArgs)}) ok`);
    } else {
      console.log(`[deploy] ${role}.new() skipped (account pre-existed)`);
    }
  }
}
