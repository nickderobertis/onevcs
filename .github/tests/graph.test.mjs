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
    lifecycle,
} from "../workflow-graph.mjs";
import { classify as classifyEvent } from "../macos-policy.mjs";
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
// Classify each derived event the way its workflow run does: a pull request by
// the real classifier over a generated git history, anything else by its name.
function classifier(h) {
    const cache = new Map();
    return (ev) => {
        if (ev.event !== "pull_request")
            return classifyEvent(null, "", "", ev.event);
        const key = JSON.stringify([ev.diff, ev.fork, ev.headBranch]);
        if (!cache.has(key))
            cache.set(
                key,
                h.classify(ev.diff, {
                    fork: ev.fork,
                    branch: ev.headBranch,
                    eventName: "pull_request",
                }),
            );
        return cache.get(key);
    };
}
// The one human action of a lifecycle: push an ordinary branch to a pull request
// and merge it. Everything after it is derived from the workflow files.
const change = (diff, fork, publication) => ({
    event: "pull_request",
    action: "synchronize",
    branch: "main",
    headBranch: "feature",
    diff,
    fork,
    ...publication,
    merge: { title: "feat: an ordinary change" },
});
test("actual workflow/call graph over event, diff, ownership and publication combinations records telemetry", () => {
    mkdirSync(".github/.telemetry", { recursive: true });
    const h = history();
    const scenarios = [];
    const classifications = new Map();
    const sources = workflows();
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
                                    releaseToken: true,
                                    upstream: sources["release.yml"].name,
                                    diagnostic:
                                        id === "manual-opt-in"
                                            ? true
                                            : undefined,
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
                                let rows = resolveScenario(
                                    s,
                                    classification,
                                    sources,
                                );
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
        const classify = classifier(h);
        for (const diff of ["source", "version", "dependency"])
            for (const fork of [false, true])
                for (const pypi of [false, true])
                    for (const npm of [false, true])
                        for (const crate of [false, true])
                            for (const releaseToken of [false, true]) {
                                const derived = lifecycle(
                                    change(diff, fork, {
                                        pypi,
                                        npm,
                                        crate,
                                        releaseToken,
                                    }),
                                    { classify, sources },
                                );
                                lifecycles.push({
                                    diff,
                                    fork,
                                    pypi,
                                    npm,
                                    crate,
                                    releaseToken,
                                    ...derived,
                                });
                            }
        // What the derivation found, for the configuration the repository runs.
        const live = (diff) =>
            lifecycles.find(
                (l) => l.diff === diff && !l.fork && l.pypi && l.npm && l.releaseToken,
            );
        assert.deepEqual(
            live("source").events.map((e) => e.label),
            [
                "pr-synchronize",
                "base-push",
                "pr-closed",
                "pr-opened",
                "release-pr-merge",
                "pr-closed",
                "release-tag",
                "release-published",
                "downstream",
            ],
        );
        assert.deepEqual(
            live("source").spending.map((s) => [s.event, s.workflow]),
            [
                ["pr-synchronize", "ci.yml"],
                ["release-published", "release.yml"],
            ],
        );
        // A version bump merged by hand is tagged by that push; no release PR follows.
        assert.equal(
            live("version").events.some((e) => e.head?.startsWith("release-plz-")),
            false,
        );
        const releaseRun = live("source").workflow_runs.find(
            (r) => r.workflow === "release.yml",
        );
        for (const form of ["archives", "wheels", "npm"])
            assert.equal(
                releaseRun.artifacts.find(
                    (a) => a.artifact === `${form}-aarch64-apple-darwin`,
                )?.producer,
                "release.yml:build-darwin",
            );
        assert.equal(
            releaseRun.artifacts.every((a) => a.scope === "tag" && a.producer),
            true,
        );
        for (const l of lifecycles)
            for (const run of l.workflow_runs)
                assert.equal(
                    run.artifacts.every((a) => a.producer),
                    true,
                    `${run.workflow} rebuilds an artifact`,
                );
        const maxJobs = Math.max(
            0,
            ...scenarios.flatMap((s) =>
                [...new Set(s.rows.map((r) => r.workflow))].map(
                    (w) =>
                        s.rows.filter((r) => r.workflow === w && r.macos)
                            .length,
                ),
            ),
            ...lifecycles.flatMap((l) =>
                l.workflow_runs.map((run) => run.mac_jobs),
            ),
        );
        const record = {
            schema_version: 2,
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

test("actual pull-request trigger defaults exclude close and draft lift from CI", () => {
    for (const action of ["closed", "ready_for_review"]) {
        const rows = resolveScenario(
            {
                event: "pull_request",
                action,
                branch: "main",
                pypi: true,
                npm: true,
                crate: true,
            },
            { macosRequired: true, isReleaseOnly: false },
        );
        assert.equal(
            rows.some((row) => row.workflow === "ci.yml"),
            false,
        );
        assert.deepEqual(
            rows.filter((row) => row.macos),
            [],
        );
    }
});

// Each edit below is to a workflow file's own trigger, condition or artifact
// dependency; the lifecycle is derived again from the edited files.
test("editing a trigger, condition or artifact dependency changes the derived runs per change", () => {
    const h = history();
    try {
        const classify = classifier(h);
        const live = { pypi: true, npm: true, crate: true, releaseToken: true };
        const runs = (sources, diff = "source") =>
            lifecycle(change(diff, false, live), { classify, sources });
        const edit = (mutate) => {
            const sources = structuredClone(workflows());
            mutate(sources);
            return sources;
        };
        const baseline = runs(workflows());
        assert.equal(baseline.runs, 2);

        // Trigger: Release also starting on the tag release-plz pushes.
        const onTag = runs(
            edit((s) => (s["release.yml"].on.push = { tags: ["v*"] })),
        );
        assert.equal(onTag.runs, 3);
        assert.deepEqual(
            onTag.spending.map((x) => x.event),
            ["pr-synchronize", "release-tag", "release-published"],
        );
        // Trigger: no automatic event fans out to Release at all.
        assert.equal(
            runs(edit((s) => (s["release.yml"].on = { workflow_dispatch: null })))
                .runs,
            1,
        );
        // Trigger: published smoke subscribed to a name nothing carries.
        const unsubscribed = runs(
            edit((s) => (s["published-smoke.yml"].on.workflow_run.workflows = ["Nope"])),
        );
        assert.equal(
            unsubscribed.events.some((e) => e.label === "downstream"),
            false,
        );

        // Condition: the diagnostic lane no longer limited to an explicit dispatch.
        const always = runs(
            edit(
                (s) =>
                    (s["published-smoke.yml"].jobs["macos-diagnostic"].if =
                        "github.event_name == 'workflow_run' || inputs.macos-diagnostic == true"),
            ),
        );
        assert.equal(always.runs, 3);
        assert.equal(always.spending.at(-1).workflow, "published-smoke.yml");
        // Condition: the Mac lane no longer asking the classifier, so the
        // version-only release PR and both base pushes spend a Mac run too.
        const unclassified = runs(
            edit(
                (s) =>
                    (s["ci.yml"].jobs["macos-pr"].if =
                        "needs.changes.outputs.rust == 'true'"),
            ),
        );
        assert.deepEqual(
            unclassified.spending.map((x) => x.event),
            [
                "pr-synchronize",
                "base-push",
                "pr-opened",
                "release-pr-merge",
                "release-published",
            ],
        );
        // Effect: an automatic dispatch cannot opt into the diagnostic unseen.
        const dispatched = runs(
            edit((s) =>
                s["release.yml"].jobs["upload-darwin"].steps.push({
                    env: { GH_TOKEN: "${{ secrets.GITHUB_TOKEN }}" },
                    run: "gh workflow run published-smoke.yml -f macos-diagnostic=true",
                }),
            ),
        );
        assert.equal(dispatched.runs, 3);
        assert.equal(
            dispatched.spending.at(-1).event,
            "manual-opt-in",
        );

        // Artifact: a downstream job reusing the release run's Darwin archive.
        const reuse = {
            "runs-on": "ubuntu-latest",
            steps: [
                {
                    uses: "actions/download-artifact@v4",
                    with: {
                        pattern: "archives-*",
                        "run-id": "${{ github.event.workflow_run.id }}",
                    },
                },
            ],
        };
        const reused = runs(
            edit((s) => (s["published-smoke.yml"].jobs.inspect = reuse)),
        );
        assert.equal(reused.runs, 2);
        const edge = reused.workflow_runs
            .find((r) => r.workflow === "published-smoke.yml")
            .artifacts.find((a) => a.artifact === "archives-aarch64-apple-darwin");
        assert.equal(edge.producer, "release.yml:build-darwin");
        assert.equal(edge.scope, "tag");
        // The same job without the upstream run's id finds nothing in its own run,
        // so a published-smoke run would have to rebuild it — on a Mac.
        const rebuilt = runs(
            edit((s) => {
                s["published-smoke.yml"].jobs.inspect = structuredClone(reuse);
                s["published-smoke.yml"].jobs.producer = {
                    "runs-on": "ubuntu-latest",
                    if: "github.event_name == 'workflow_dispatch'",
                    steps: [
                        {
                            uses: "actions/upload-artifact@v4",
                            with: { name: "archives-aarch64-apple-darwin" },
                        },
                    ],
                };
                s["published-smoke.yml"].jobs.inspect.needs = ["producer"];
                s["published-smoke.yml"].jobs.inspect.if = "always()";
                delete s["published-smoke.yml"].jobs.inspect.steps[0].with["run-id"];
            }),
        );
        assert.equal(rebuilt.runs, 3);
        assert.equal(rebuilt.spending.at(-1).jobs[0].rebuild, true);
        // In the release run: Darwin wheels no longer reached through `needs`.
        const unordered = runs(
            edit((s) =>
                (s["release.yml"].jobs["publish-pypi"].needs = ["build-wheels"]),
            ),
        );
        assert.equal(
            unordered.spending.find((x) => x.workflow === "release.yml").jobs
                .length,
            2,
        );
        // A download nothing could have produced is an error, not a zero.
        assert.throws(
            () =>
                runs(
                    edit((s) =>
                        s["release.yml"].jobs["upload-darwin"].steps.unshift({
                            uses: "actions/download-artifact@v4",
                            with: { pattern: "missing-*" },
                        }),
                    ),
                ),
            /artifact missing-\* has no producer/,
        );
        // Filters the model does not evaluate are refused rather than ignored.
        assert.throws(
            () =>
                runs(
                    edit((s) => (s["ci.yml"].on.push.paths = ["crates/**"])),
                ),
            /unsupported push filter paths/,
        );
    } finally {
        h.cleanup();
    }
});
