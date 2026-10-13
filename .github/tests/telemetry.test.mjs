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
        assert.notEqual(semantic.status, 0);
        assert.equal(semantic.stdout, "");
        assert.match(semantic.stderr, /expected one semantic file path[\s\S]*ACTION:/);
    } finally {
        rmSync(dir, { recursive: true, force: true });
    }
});

test("budget verdict adapter validates real checker JSON outcomes", () => {
    for (const report of [
        null,
        [],
        {},
        { results: [null] },
        { results: [{ id: [], verdict: "within", error: null }] },
        { results: [{ id: "budget", verdict: "within", error: {} }] },
        {
            results: [
                { id: "budget", verdict: "error", error: "failed measurement" },
            ],
        },
        { results: [{ id: "budget", verdict: "over", error: null }] },
    ]) {
        const result = spawnSync("node", [".github/budget-verdict.mjs"], {
            input: JSON.stringify(report),
            encoding: "utf8",
        });
        assert.notEqual(result.status, 0);
        assert.match(result.stderr, /ACTION:/);
    }
    const good = spawnSync("node", [".github/budget-verdict.mjs"], {
        input: JSON.stringify({
            schema_version: 1,
            results: [{ id: "budget", verdict: "within", error: null }],
        }),
        encoding: "utf8",
    });
    assert.equal(good.status, 0);
    assert.match(good.stdout, /within budget/);
});
