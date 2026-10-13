import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { resolve, join } from "node:path";
test("reporting commands reject missing or malformed telemetry without rerunning workflows", () => {
    const base =
        process.env.ONEPIPELINE_NODE_SCRATCH_DIR ||
        resolve(".github/.telemetry");
    mkdirSync(base, { recursive: true });
    const dir = mkdtempSync(join(base, "telemetry-"));
    try {
        const path = join(dir, "record.json");
        for (const value of [
            undefined,
            null,
            [],
            {},
            { event_classes: [], scenarios: [], lifecycles: [], metrics: [] },
        ]) {
            if (value !== undefined) writeFileSync(path, JSON.stringify(value));
            const result = spawnSync(
                "node",
                [
                    ".github/macos-budget.mjs",
                    "--metric",
                    "jobs-per-run",
                    "--telemetry",
                    path,
                ],
                { encoding: "utf8" },
            );
            assert.notEqual(result.status, 0);
            assert.match(result.stderr, /ACTION: run github-workflows:test/);
        }
        const semantic = spawnSync("python3", [".github/version-only.py"], {
            input: JSON.stringify({
                before: '[package]\nname="onevcs"\nversion="0.1.0"',
                after: '[package]\nname="onevcs"\nversion="0.2.0"',
            }),
            encoding: "utf8",
        });
        assert.equal(semantic.status, 0);
        assert.equal(semantic.stdout.trim(), "false");
        assert.equal(semantic.stderr, "");
    } finally {
        rmSync(dir, { recursive: true, force: true });
    }
});
