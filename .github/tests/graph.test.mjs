import test from "node:test";
import assert from "node:assert/strict";
import { mkdirSync, writeFileSync, readFileSync } from "node:fs";
import {
    resolveScenario,
    resolveRun,
    workflows,
    matrixRows,
    sourceDigest,
    expression,
} from "../workflow-graph.mjs";
import { history } from "./policy.test.mjs";
const classes = [
    ["pr-opened", "pull_request", "opened", "main"],
    ["pr-synchronize", "pull_request", "synchronize", "main"],
    ["pr-reopened", "pull_request", "reopened", "main"],
    ["base-push", "push", null, "main"],
    ["release-pr-merge", "push", null, "main"],
    ["release-tag", "push", null, "tag"],
    ["release-published", "release", "published", "tag"],
    ["downstream", "workflow_run", "completed", "main"],
    ["repository-dispatch", "repository_dispatch", null, "main"],
    ["scheduled", "schedule", null, "main"],
    ["manual-default", "workflow_dispatch", null, "main"],
    ["manual-opt-in", "workflow_dispatch", null, "main"],
];
const protectedNames = [
    "cross (macos-latest)",
    "cross (windows-latest)",
    "deny",
    "gate",
    "install (macos-latest)",
    "install (ubuntu-latest)",
    "install (windows-latest)",
    "llmlint",
    "msrv",
    "pr-title",
    "visual-docs / report (x86_64, shots/current, shots/verify, Visual docs)",
];
test("actual workflow/call graph over event, diff, ownership and publication combinations records telemetry", () => {
    mkdirSync(".github/.telemetry", { recursive: true });
    const h = history();
    const scenarios = [];
    const classifications = new Map();
    try {
        for (const [id, event, action, branch] of classes)
            for (const diff of ["source", "version", "dependency"])
                for (const fork of [false, true])
                    for (const pypi of [false, true])
                        for (const npm of [false, true])
                            for (const crate of [false, true]) {
                                const s = {
                                    id,
                                    event,
                                    action,
                                    branch,
                                    diff,
                                    fork,
                                    pypi,
                                    npm,
                                    crate,
                                    upstream: "Release",
                                };
                                const key = JSON.stringify([diff, fork, event]);
                                if (!classifications.has(key))
                                    classifications.set(
                                        key,
                                        h.classify(diff, {
                                            fork,
                                            branch:
                                                diff === "version"
                                                    ? "release-plz-2026"
                                                    : "feature",
                                            eventName: event,
                                        }),
                                    );
                                const classification = classifications.get(key);
                                let rows = resolveScenario(s, classification);
                                // Manual boolean inputs are resolved by the workflow, default off.
                                if (event === "workflow_dispatch") {
                                    const wf = structuredClone(
                                        workflows()["published-smoke.yml"],
                                    );
                                    wf.on.workflow_call = {
                                        inputs: {
                                            "macos-diagnostic": {
                                                default: id === "manual-opt-in",
                                            },
                                        },
                                    };
                                    rows = resolveRun(
                                        "published-smoke.yml",
                                        wf,
                                        s,
                                        classification,
                                    );
                                }
                                if (event === "pull_request") {
                                    const names = rows.map((r) => r.name);
                                    for (const name of protectedNames)
                                        assert.equal(
                                            names.filter((n) => n === name)
                                                .length,
                                            1,
                                            `missing/duplicate required context ${name}: ${id}/${diff}/${fork}`,
                                        );
                                    const mac = rows.filter((r) => r.macos);
                                    assert.deepEqual(
                                        mac.map((r) => r.job),
                                        classification.macosRequired
                                            ? ["macos-pr"]
                                            : [],
                                    );
                                }
                                if (
                                    ![
                                        "pull_request",
                                        "release",
                                        "workflow_dispatch",
                                    ].includes(event)
                                )
                                    assert.deepEqual(
                                        rows.filter((r) => r.macos),
                                        [],
                                        `${id} requests Mac`,
                                    );
                                if (
                                    event === "workflow_dispatch" &&
                                    id === "manual-default"
                                )
                                    assert.deepEqual(
                                        rows.filter((r) => r.macos),
                                        [],
                                    );
                                if (event === "release")
                                    assert.deepEqual(
                                        rows
                                            .filter((r) => r.macos)
                                            .map((r) => r.job),
                                        ["build-darwin"],
                                    );
                                scenarios.push({ ...s, classification, rows });
                            }
        const lifecycles = [];
        for (const diff of ["source", "version", "dependency"])
            for (const fork of [false, true])
                for (const pypi of [false, true])
                    for (const npm of [false, true])
                        for (const crate of [false, true]) {
                            const selects = [
                                "pr-synchronize",
                                "base-push",
                                "release-pr-merge",
                                "release-tag",
                                "release-published",
                                "downstream",
                            ];
                            const sequence = selects.map((id) =>
                                scenarios.find(
                                    (s) =>
                                        s.id === id &&
                                        s.diff === diff &&
                                        s.fork === fork &&
                                        s.pypi === pypi &&
                                        s.npm === npm &&
                                        s.crate === crate,
                                ),
                            );
                            // A release-only PR is a separate run; its merge and publication share the tag tree.
                            sequence.splice(
                                2,
                                0,
                                scenarios.find(
                                    (s) =>
                                        s.id === "pr-opened" &&
                                        s.diff === "version" &&
                                        !s.fork &&
                                        s.pypi === pypi &&
                                        s.npm === npm &&
                                        s.crate === crate,
                                ),
                            );
                            const spending = sequence.flatMap((s, index) =>
                                [
                                    ...new Set(
                                        s.rows
                                            .filter((r) => r.macos)
                                            .map((r) => r.workflow),
                                    ),
                                ].map((workflow) => ({
                                    event: s.id,
                                    workflow,
                                    index,
                                    jobs: s.rows.filter(
                                        (r) =>
                                            r.macos && r.workflow === workflow,
                                    ),
                                })),
                            );
                            lifecycles.push({
                                diff,
                                fork,
                                pypi,
                                npm,
                                crate,
                                runs: spending.length,
                                spending,
                            });
                        }
        const maxJobs = Math.max(
            0,
            ...scenarios.flatMap((s) =>
                [...new Set(s.rows.map((r) => r.workflow))].map(
                    (w) =>
                        s.rows.filter((r) => r.workflow === w && r.macos)
                            .length,
                ),
            ),
        );
        const record = {
            schema_version: 1,
            source_digest: sourceDigest(),
            event_classes: classes.map((c) => c[0]),
            scenarios,
            lifecycles,
            metrics: {
                jobs_per_run: maxJobs,
                runs_per_change: Math.max(...lifecycles.map((l) => l.runs)),
            },
        };
        writeFileSync(
            ".github/.telemetry/macos-policy.json",
            JSON.stringify(record, null, 2) + "\n",
        );
        console.log(
            `recorded ${scenarios.length} scenarios; max jobs/run ${maxJobs}; max runs/change ${record.metrics.runs_per_change}`,
        );
    } finally {
        h.cleanup();
    }
});
test("unknown runner/call/matrix expressions are explicit failures", () => {
    const s = { event: "pull_request", pypi: true, npm: true };
    for (const job of [
        { "runs-on": "${{ missing.runner }}" },
        { uses: "someone/repo/.github/workflows/unknown.yml@v1" },
        {
            "runs-on": "ubuntu-latest",
            strategy: {
                matrix: {
                    include: "${{ fromJSON(needs.unknown.outputs.matrix) }}",
                },
            },
        },
    ])
        assert.throws(() =>
            resolveRun("broken", { jobs: { broken: job } }, s, {
                macosRequired: true,
            }),
        );
    assert.deepEqual(
        matrixRows(
            {
                os: ["ubuntu", "windows"],
                include: [
                    { os: "windows", target: "x64" },
                    { os: "macos", target: "arm64" },
                ],
            },
            {},
        ),
        [
            { os: "ubuntu" },
            { os: "windows", target: "x64" },
            { os: "macos", target: "arm64" },
        ],
    );
});
test("real job conditions keep carriers on failed prerequisites and path skips honest", () => {
    const wf = workflows()["ci.yml"];
    for (const result of ["success", "failure", "cancelled", "skipped"])
        for (const lane of ["cross-macos", "install-macos"]) {
            assert.equal(
                expression(wf.jobs[lane].if, {
                    needs: {
                        changes: { result },
                        "macos-pr": { result },
                        gate: { result },
                    },
                }),
                true,
            );
        }
    const rows = resolveRun(
        "ci.yml",
        wf,
        { event: "pull_request", rust: false, pypi: false, npm: false },
        { macosRequired: true },
    );
    assert.equal(rows.find((r) => r.job === "macos-pr").scheduled, false);
    assert.equal(rows.find((r) => r.job === "cross-macos").scheduled, true);
});
