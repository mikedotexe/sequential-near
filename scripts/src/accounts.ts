import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { KeyPair, utils } from "near-api-js";

import {
  MASTER_ACCOUNT_ID,
  NEAR_NETWORK,
  NETWORK_CREDENTIALS_DIR,
} from "./config.js";
import { accountExists, connectSender } from "./rpc.js";

function credentialPath(accountId: string): string {
  return join(NETWORK_CREDENTIALS_DIR, `${accountId}.json`);
}

export function assertMasterCredentialPresent(): void {
  const path = credentialPath(MASTER_ACCOUNT_ID);
  if (!existsSync(path)) {
    throw new Error(
      `Missing credentials for ${MASTER_ACCOUNT_ID} on ${NEAR_NETWORK}: ` +
        `expected key file at ${path}. ` +
        `Create via \`near-cli-rs\` or similar and place the JSON there.`,
    );
  }
}

export function readCredential(accountId: string): KeyPair {
  const path = credentialPath(accountId);
  if (!existsSync(path)) {
    throw new Error(`missing credential file: ${path}`);
  }
  const parsed = JSON.parse(readFileSync(path, "utf8")) as {
    account_id: string;
    public_key: string;
    private_key: string;
  };
  return KeyPair.fromString(parsed.private_key as utils.KeyPairString);
}

export function writeCredential(accountId: string, keyPair: KeyPair): void {
  mkdirSync(NETWORK_CREDENTIALS_DIR, { recursive: true });
  const publicKey = keyPair.getPublicKey().toString();
  const privateKey = keyPair.toString();
  const json = { account_id: accountId, public_key: publicKey, private_key: privateKey };
  writeFileSync(credentialPath(accountId), JSON.stringify(json));
}

export function removeCredential(accountId: string): void {
  const path = credentialPath(accountId);
  if (existsSync(path)) {
    rmSync(path);
  }
}

/// Create a sub-account of the master and write its key to the
/// credentials directory. Idempotent-ish: if the account already
/// exists we assume its key is on disk and skip.
export async function ensureSubAccount(
  accountId: string,
  initialBalanceNear: string,
): Promise<"existed" | "created"> {
  if (await accountExists(accountId)) {
    return "existed";
  }
  const near = await connectSender();
  const master = await near.account(MASTER_ACCOUNT_ID);
  const keyPair = KeyPair.fromRandom("ed25519");
  const balanceYocto = utils.format.parseNearAmount(initialBalanceNear);
  if (!balanceYocto) {
    throw new Error(`failed to parse NEAR amount: ${initialBalanceNear}`);
  }
  await master.createAccount(accountId, keyPair.getPublicKey(), BigInt(balanceYocto));
  writeCredential(accountId, keyPair);
  return "created";
}

/// Destroy a sub-account, routing its remaining balance to the master.
/// Removes the credential file too. Idempotent: no-op if the account
/// doesn't exist.
export async function destroySubAccount(accountId: string): Promise<"existed" | "absent"> {
  if (!(await accountExists(accountId))) {
    removeCredential(accountId);
    return "absent";
  }
  const near = await connectSender();
  const account = await near.account(accountId);
  await account.deleteAccount(MASTER_ACCOUNT_ID);
  removeCredential(accountId);
  return "existed";
}
