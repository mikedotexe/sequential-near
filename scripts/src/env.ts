import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";

// Tiny, dependency-free .env loader. Reads `<repo-root>/.env` if present and
// sets any keys that aren't already in `process.env`. Values already set in
// the shell win — same precedence as every other dotenv loader.

export function loadDotEnv(repoRoot: string): void {
  const path = join(repoRoot, ".env");
  if (!existsSync(path)) return;
  const raw = readFileSync(path, "utf8");
  for (const line of raw.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) continue;
    const eq = trimmed.indexOf("=");
    if (eq < 0) continue;
    const key = trimmed.slice(0, eq).trim();
    let value = trimmed.slice(eq + 1).trim();
    if (
      (value.startsWith('"') && value.endsWith('"')) ||
      (value.startsWith("'") && value.endsWith("'"))
    ) {
      value = value.slice(1, -1);
    }
    if (!(key in process.env)) {
      process.env[key] = value;
    }
  }
}
