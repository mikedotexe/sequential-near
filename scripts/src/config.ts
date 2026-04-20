import { homedir } from "node:os";
import { join } from "node:path";

import { loadDotEnv } from "./env.js";

const REPO_ROOT_FOR_ENV = new URL("../../", import.meta.url).pathname.replace(/\/$/, "");
loadDotEnv(REPO_ROOT_FOR_ENV);

export const FASTNEAR_API_KEY = process.env.FASTNEAR_API_KEY ?? "";

const rawNetwork = (process.env.NEAR_NETWORK ?? "testnet").toLowerCase();
if (rawNetwork !== "testnet" && rawNetwork !== "mainnet") {
  throw new Error(`NEAR_NETWORK must be "testnet" or "mainnet", got "${rawNetwork}"`);
}
export const NEAR_NETWORK: "testnet" | "mainnet" = rawNetwork;

const NETWORK_DEFAULTS = {
  testnet: {
    masterAccount: "mike.testnet",
    rpcSend: "https://rpc.testnet.fastnear.com",
    rpcAudit: "https://archival-rpc.testnet.fastnear.com",
    explorerBase: "https://testnet.nearblocks.io",
    expectedChainId: "testnet",
  },
  mainnet: {
    masterAccount: "mike.near",
    rpcSend: "https://rpc.mainnet.fastnear.com",
    rpcAudit: "https://archival-rpc.mainnet.fastnear.com",
    explorerBase: "https://nearblocks.io",
    expectedChainId: "mainnet",
  },
} as const;

const DEFAULTS = NETWORK_DEFAULTS[NEAR_NETWORK];

export const MASTER_ACCOUNT_ID = process.env.MASTER_ACCOUNT_ID ?? DEFAULTS.masterAccount;

// Flat sibling layout under the master account:
//
//   mike.{net}
//   ├── gate.mike.{net}       ← signed-intent sequencer
//   ├── register.mike.{net}   ← non-commutative target
//   ├── ft.mike.{net}         ← FT-like target
//   ├── alice.mike.{net}      ← user who signs delegates
//   ├── relayer.mike.{net}    ← submits intents to the gate
//   └── approver.mike.{net}   ← coordinator who resumes
//
// Each role gets its own keypair so access-control paths in the gate are
// testable end-to-end (relayer whitelist, approver-only resume, etc.).
export const ACCOUNTS = {
  gate: process.env.ACCOUNT_GATE ?? `gate.${MASTER_ACCOUNT_ID}`,
  register: process.env.ACCOUNT_REGISTER ?? `register.${MASTER_ACCOUNT_ID}`,
  ftShim: process.env.ACCOUNT_FT_SHIM ?? `ft.${MASTER_ACCOUNT_ID}`,
  alice: process.env.ACCOUNT_ALICE ?? `alice.${MASTER_ACCOUNT_ID}`,
  relayer: process.env.ACCOUNT_RELAYER ?? `relayer.${MASTER_ACCOUNT_ID}`,
  approver: process.env.ACCOUNT_APPROVER ?? `approver.${MASTER_ACCOUNT_ID}`,
} as const;

export type AccountKey = keyof typeof ACCOUNTS;

export const WASM_PATHS: Record<"gate" | "register" | "ftShim", string> = {
  gate: "target/wasm32-unknown-unknown/release/gate.wasm",
  register: "target/wasm32-unknown-unknown/release/register.wasm",
  ftShim: "target/wasm32-unknown-unknown/release/ft_shim.wasm",
};

export const RPC_SEND = process.env.RPC_SEND ?? DEFAULTS.rpcSend;
export const RPC_AUDIT = process.env.RPC_AUDIT ?? DEFAULTS.rpcAudit;

export const EXPLORER_BASE = DEFAULTS.explorerBase;
export const EXPECTED_CHAIN_ID = DEFAULTS.expectedChainId;

export const CREDENTIALS_DIR =
  process.env.NEAR_CREDENTIALS_DIR ?? join(homedir(), ".near-credentials");
export const NETWORK_CREDENTIALS_DIR = join(CREDENTIALS_DIR, NEAR_NETWORK);

export const REPO_ROOT = new URL("../../", import.meta.url).pathname.replace(/\/$/, "");
export const RUNS_ROOT = join(REPO_ROOT, "runs");
export const RUNS_DIR = join(RUNS_ROOT, NEAR_NETWORK);

// Per-contract initial balance. Contracts are small (~150–340 KiB);
// 3 NEAR covers storage + plenty of runtime headroom. Alice + relayer +
// approver are non-contract accounts; 1 NEAR is more than enough for gas.
export const CONTRACT_INITIAL_BALANCE_NEAR = process.env.CONTRACT_INITIAL_BALANCE_NEAR ?? "3";
export const USER_INITIAL_BALANCE_NEAR = process.env.USER_INITIAL_BALANCE_NEAR ?? "1";

// FT-shim total supply at init. 10^24 base units — generous headroom
// for a demo. The owner at init is the gate account (so the gate has
// balance to transfer on behalf of users via the dispatch path).
export const FT_SHIM_TOTAL_SUPPLY = process.env.FT_SHIM_TOTAL_SUPPLY ?? "1000000000000000000000000";

// Gas budgets for outer tx calls. submit_intent creates a yield with
// GAS_YIELD_CALLBACK=200 Tgas reserved for the callback — that reservation
// must fit inside the tx's attached gas, so submit needs comfortably more
// than 200 (preamble: sig verify + state writes). 300 gives generous
// headroom. Resume calls don't create a new yield reservation (they deliver
// a payload to an already-yielded callback), so 150 is plenty.
export const GAS_SUBMIT_TGAS = 300;
export const GAS_RESUME_TGAS = 150;
export const GAS_DELEGATE_INNER_TGAS = 30;
