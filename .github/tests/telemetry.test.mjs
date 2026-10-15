import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { resolve, join } from "node:path";
import { sourceDigest } from "../workflow-graph.mjs";
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
        // Complete in shape, so only the named check can be what refuses it.
        const shaped = {
            schema_version: 2,
            source_digest: sourceDigest(),
            event_classes: Array.from({ length: 12 }, (_, i) => `class-${i}`),
            scenarios: [{ rows: [] }],
            lifecycles: [{ spending: [], events: [], workflow_runs: [] }],
            metrics: { jobs_per_run: 0, runs_per_change: 0 },
        };
        const budget = (record) => {
            writeFileSync(path, JSON.stringify(record));
            return spawnSync(
                "node",
                [
                    ".github/macos-budget.mjs",
                    "--metric",
                    "runs-per-change",
                    "--telemetry",
                    path,
                ],
                { encoding: "utf8", env: { ...process.env, ONEBUDGETSPEC_RESULT: "" } },
            );
        };
        assert.match(budget(shaped).stderr, /invoke through onebudgetspec/);
        assert.match(
            budget({ ...shaped, schema_version: 1 }).stderr,
            /missing\/stale\/incomplete/,
        );
        assert.match(
            budget({ ...shaped, source_digest: "0".repeat(64) }).stderr,
            /missing\/stale\/incomplete/,
        );
        for (const field of ["events", "workflow_runs", "spending"]) {
            const lifecycle = { ...shaped.lifecycles[0] };
            delete lifecycle[field];
            const result = budget({ ...shaped, lifecycles: [lifecycle] });
            assert.notEqual(result.status, 0);
            assert.match(result.stderr, /lifecycle events\/runs\/spending must be arrays/);
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
