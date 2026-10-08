#!/usr/bin/env node
// Invalidate old evidence before compilation/test startup; failed producers leave it incomplete.
import { mkdirSync, writeFileSync, rmSync } from "node:fs";
import { randomBytes } from "node:crypto";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const directory = fileURLToPath(new URL("../target/budget-records/", import.meta.url));
mkdirSync(directory, { recursive: true });
const run_id = randomBytes(32).toString("hex");
const manifest = state => writeFileSync(`${directory}/recoverable-invocation.json`, JSON.stringify({ state, run_id }));
const failed = reason => {
  manifest("failed");
  rmSync(`${directory}/recoverable.json`, { force: true });
  console.error(`recovery producer: ${reason}\nnext: run 'just recoverable-journeys' to regenerate complete current-build records`);
};
manifest("started");
rmSync(`${directory}/recoverable.json`, { force: true });
const [program, ...args] = process.argv.slice(2);
if (!program) {
  failed("producer command is missing");
  process.exitCode = 2;
} else {
  const result = spawnSync(program, args, { stdio: "inherit", env: { ...process.env, ONEVCS_RECOVERY_INVOCATION: run_id } });
  if (result.error || result.status !== 0) {
    failed(result.error?.message ?? `${program} ${args.join(" ")} failed with ${result.signal ? `signal ${result.signal}` : `exit ${result.status}`}`);
  }
  process.exitCode = result.status ?? 1;
}
