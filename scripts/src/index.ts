#!/usr/bin/env tsx
import { cmdClean } from "./commands/clean.js";
import { cmdDeploy } from "./commands/deploy.js";
import { cmdSequence } from "./commands/sequence.js";
import { cmdSubmit, type SubmitTarget, type SubmitVariant } from "./commands/submit.js";

const HELP = `sequential — signed-intent sequencer workflow

Usage:
  tsx src/index.ts <command> [flags]

Commands:
  deploy                         create accounts + deploy contracts
  clean                          destroy all accounts (routes balance to master)
  submit [--variant <v>] [--target <t>]
                                 end-to-end single-intent flow
                                 variant: claim | reject | timeout | bad-sig | expired | replay
                                 target:  register (default) | ft-shim
  sequence --n <N> [--target <t>] [--permutation identity|random|<csv>]
                                 N-intent chained batch with permutation check
                                 target:       register (default) | ft-shim
                                 permutation:  identity (default) | random | comma-separated

Flags:
  --i-know-this-is-testnet       required for \`clean\` on testnet
  --i-know-this-is-mainnet       required for \`deploy\` and \`clean\` on mainnet

Env:
  NEAR_NETWORK=testnet|mainnet    default: testnet
  MASTER_ACCOUNT_ID               default: mike.{testnet,near}
  FASTNEAR_API_KEY                optional; raises rate limits
`;

function parseFlag(name: string, defaultValue: string): string {
  const args = process.argv.slice(3);
  const idx = args.indexOf(`--${name}`);
  if (idx === -1 || idx + 1 >= args.length) return defaultValue;
  return args[idx + 1] ?? defaultValue;
}

const VALID_VARIANTS: SubmitVariant[] = [
  "claim",
  "reject",
  "timeout",
  "bad-sig",
  "expired",
  "replay",
];
const VALID_TARGETS: SubmitTarget[] = ["register", "ft-shim"];

async function main(): Promise<void> {
  const [, , cmd] = process.argv;
  switch (cmd) {
    case "deploy":
      await cmdDeploy();
      break;
    case "clean":
      await cmdClean();
      break;
    case "submit": {
      const variant = parseFlag("variant", "claim");
      const target = parseFlag("target", "register");
      if (!VALID_VARIANTS.includes(variant as SubmitVariant)) {
        throw new Error(`unknown --variant: ${variant}. valid: ${VALID_VARIANTS.join(", ")}`);
      }
      if (!VALID_TARGETS.includes(target as SubmitTarget)) {
        throw new Error(`unknown --target: ${target}. valid: ${VALID_TARGETS.join(", ")}`);
      }
      await cmdSubmit({
        variant: variant as SubmitVariant,
        target: target as SubmitTarget,
      });
      break;
    }
    case "sequence": {
      const nStr = parseFlag("n", "3");
      const n = parseInt(nStr, 10);
      if (!Number.isFinite(n) || n < 2) {
        throw new Error(`--n must be an integer >= 2, got ${nStr}`);
      }
      const target = parseFlag("target", "register");
      if (!VALID_TARGETS.includes(target as SubmitTarget)) {
        throw new Error(`unknown --target: ${target}. valid: ${VALID_TARGETS.join(", ")}`);
      }
      const permutation = parseFlag("permutation", "identity");
      await cmdSequence({
        n,
        target: target as SubmitTarget,
        permutation,
      });
      break;
    }
    case undefined:
    case "help":
    case "--help":
    case "-h":
      process.stdout.write(HELP);
      return;
    default:
      process.stderr.write(`unknown command: ${cmd}\n\n${HELP}`);
      process.exit(1);
  }
}

main().catch((err) => {
  process.stderr.write(`error: ${err.message ?? err}\n`);
  if (process.env.DEBUG) {
    process.stderr.write(`${err.stack ?? ""}\n`);
  }
  process.exit(1);
});
