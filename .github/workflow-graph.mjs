// Repository-private scheduling model. Expressions use Actions' published parser
// and evaluator; an unknown runner, matrix output or reusable call is an error.
import {
    readFileSync,
    readdirSync,
    mkdirSync,
    mkdtempSync,
    rmSync,
} from "node:fs";
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import YAML from "yaml";
import { Lexer, Parser, Evaluator, data } from "@actions/expressions";

export function expression(source, context) {
    if (typeof source !== "string") return source;
    const body = source.trim().replace(/^\$\{\{\s*|\s*\}\}$/g, "");
    const funcs = ["always", "success", "failure", "cancelled"].map((name) => ({
        name,
        minArgs: 0,
        maxArgs: 0,
    }));
    const ast = new Parser(
        new Lexer(body).lex().tokens,
        Object.keys(context),
        funcs,
    ).parse();
    const outcomes = Object.values(context.needs || {}).map((n) => n.result);
    const functions = new Map(
        funcs.map((f) => [
            f.name,
            {
                ...f,
                call: () =>
                    new data.BooleanData(
                        f.name === "always" ||
                            (f.name === "success" &&
                                outcomes.every((r) => r === "success")) ||
                            (f.name === "failure" &&
                                outcomes.includes("failure")) ||
                            (f.name === "cancelled" &&
                                outcomes.includes("cancelled")),
                    ),
            },
        ]),
    );
    const result = new Evaluator(
        ast,
        JSON.parse(JSON.stringify(context), data.reviver),
        functions,
    ).evaluate();
    return JSON.parse(JSON.stringify(result, data.replacer));
}
function interpolate(value, context) {
    if (typeof value !== "string") return value;
    if (/^\$\{\{[^]*\}\}$/.test(value)) return expression(value, context);
    return value.replace(/\$\{\{([^]*?)\}\}/g, (_, body) =>
        String(expression(body, context)),
    );
}
export function matrixRows(matrix, context) {
    if (!matrix) return [{}];
    const resolved =
        typeof matrix === "string" ? expression(matrix, context) : matrix;
    if (!resolved || typeof resolved !== "object")
        throw new Error("unresolved matrix");
    let rows = [{}];
    const axes = Object.keys(resolved).filter(
        (k) => !["include", "exclude"].includes(k),
    );
    for (const key of axes) {
        const values = interpolate(resolved[key], context);
        if (!Array.isArray(values) || !values.length)
            throw new Error(`unresolved/empty matrix axis ${key}`);
        rows = rows.flatMap((row) =>
            values.map((value) => ({ ...row, [key]: value })),
        );
    }
    if (!axes.length) {
        const included = interpolate(resolved.include || [], context);
        if (!Array.isArray(included) || !included.length)
            throw new Error("unresolved/empty include matrix");
        return included;
    }
    for (const included of interpolate(resolved.include || [], context)) {
        if (!included || typeof included !== "object")
            throw new Error("unresolved matrix include");
        let merged = false;
        for (let i = 0; i < rows.length; i++) {
            if (
                axes.every(
                    (key) =>
                        !(key in included) || rows[i][key] === included[key],
                )
            ) {
                rows[i] = { ...rows[i], ...included };
                merged = true;
            }
        }
        if (!merged) rows.push(included);
    }
    return rows.filter(
        (row) =>
            !(resolved.exclude || []).some((excluded) =>
                Object.entries(excluded).every(
                    ([key, val]) => row[key] === val,
                ),
            ),
    );
}
export function workflows() {
    return Object.fromEntries(
        readdirSync(".github/workflows")
            .filter((p) => /\.ya?ml$/.test(p))
            .sort()
            .map((p) => [
                p,
                YAML.parse(readFileSync(`.github/workflows/${p}`, "utf8")),
            ]),
    );
}
export function sourceDigest() {
    const hash = createHash("sha256");
    const paths = execFileSync("git", ["ls-files", "-z"], { encoding: "utf8" })
        .split("\0")
        .filter(Boolean)
        .sort();
    for (const path of paths) {
        hash.update(path + "\0");
        hash.update(readFileSync(path));
    }
    return hash.digest("hex");
}
export function glob(pattern, name) {
    const body = pattern
        .split("*")
        .map((part) => part.replace(/[.+?^${}()|[\]\\]/g, "\\$&"))
        .join(".*");
    return new RegExp(`^${body}$`).test(name);
}
function triggered(workflow, scenario, file) {
    // A dispatch names the one workflow it starts.
    if (scenario.target && scenario.target !== file) return false;
    const on = workflow.on;
    if (typeof on === "string") return on === scenario.event;
    if (Array.isArray(on)) return on.includes(scenario.event);
    if (!on || !(scenario.event in on)) return false;
    const spec = on[scenario.event];
    for (const filter of ["paths", "paths-ignore", "branches-ignore", "tags-ignore"])
        if (spec?.[filter])
            throw new Error(
                `${file}: unsupported ${scenario.event} filter ${filter}`,
            );
    // Actions' default pull_request activity set excludes close and draft lift.
    if (
        scenario.event === "pull_request" &&
        !spec?.types &&
        !["opened", "synchronize", "reopened"].includes(scenario.action)
    )
        return false;
    if (spec?.types && !spec.types.includes(scenario.action)) return false;
    if (scenario.event === "push") {
        // A push filtered only by branches ignores tags, and the reverse.
        const tag = scenario.branch === "tag";
        const filter = tag ? spec?.tags : spec?.branches;
        const other = tag ? spec?.branches : spec?.tags;
        if (!filter) return !other;
        const name = tag ? scenario.tag || "v1.2.3" : scenario.branch;
        return filter.some((pattern) => glob(pattern, name));
    }
    if (spec?.branches && !spec.branches.some((p) => glob(p, scenario.branch)))
        return false;
    if (spec?.workflows && !spec.workflows.includes(scenario.upstream))
        return false;
    return true;
}
const inventory = JSON.parse(
    readFileSync(".github/vendor/workflow-sources.json", "utf8"),
);
function reusable(call) {
    if (call.startsWith("./"))
        return YAML.parse(readFileSync(call.slice(2), "utf8"));
    const item = inventory[call];
    if (!item?.sha256 || typeof item?.source !== "string")
        throw new Error(`unresolved reusable workflow ${call}`);
    const parts = call.match(/^([^/]+)\/([^/]+)\/(.+)@([^@]+)$/);
    if (!parts || item.url !== `https://raw.githubusercontent.com/${parts[1]}/${parts[2]}/${parts[4]}/${parts[3]}`)
        throw new Error(`vendored reusable source URL mismatch for ${call}`);
    if (createHash("sha256").update(item.source).digest("hex") !== item.sha256)
        throw new Error(`vendored reusable source SHA-256 mismatch for ${call}`);
    return YAML.parse(item.source);
}
const matrixCache = new Map();
function screencompMatrix(inputs) {
    const config = readFileSync("screencomp.toml", "utf8");
    const key = JSON.stringify([config, inputs]);
    if (matrixCache.has(key)) return matrixCache.get(key);
    const wf = reusable(
        "nickderobertis/screencomp/.github/workflows/visual-docs-reusable.yml@v0.4.8",
    );
    const step = wf.jobs.arches.steps.find((s) => s.id === "read");
    if (!step?.run) throw new Error("unresolved screencomp matrix producer");
    const scratch =
        process.env.ONEPIPELINE_NODE_SCRATCH_DIR || ".github/.telemetry";
    mkdirSync(scratch, { recursive: true });
    const dir = mkdtempSync(scratch + "/matrix-");
    try {
        // Double the external screencomp CLI at its documented config-read boundary;
        // execute the pinned producer's real jq matrix assembly and output command.
        const cli = `screencomp() { python3 -c 'import json,tomllib;print(json.dumps(tomllib.load(open("screencomp.toml","rb"))["capture"]["arches"]))'; };`;
        execFileSync("bash", ["-c", cli + "\n" + step.run], {
            env: {
                ...process.env,
                ARCHES_OVERRIDE: inputs.arches || "",
                PROJECTS_INPUT: inputs.projects || "[]",
                DEFAULT_TITLE: inputs["gallery-title"],
                GITHUB_OUTPUT: dir + "/output",
            },
        });
        const line = readFileSync(dir + "/output", "utf8").trim();
        if (!line.startsWith("matrix="))
            throw new Error("unresolved screencomp matrix output");
        const matrix = JSON.parse(line.slice(7));
        matrixCache.set(key, matrix);
        return matrix;
    } finally {
        rmSync(dir, { recursive: true, force: true });
    }
}
export function resolveRun(
    file,
    workflow,
    scenario,
    classification,
    overrides = {},
) {
    const rows = [];
    const github = {
        event_name: scenario.event,
        event: {
            action: scenario.action,
            head_commit: { message: scenario.headCommit ?? "" },
            pull_request: {
                head: {
                    repo: {
                        full_name: scenario.fork
                            ? "someone/fork"
                            : "nickderobertis/onevcs",
                    },
                },
            },
            workflow_run: { conclusion: "success", id: 1 },
        },
        repository: "nickderobertis/onevcs",
        token: "github-token",
        head_ref: scenario.headBranch ?? "",
        ref:
            scenario.branch === "tag"
                ? `refs/tags/${scenario.tag || "v1.2.3"}`
                : "refs/heads/main",
        ref_name: scenario.tag || "v1.2.3",
    };
    const vars = {
        PYPI_PUBLISH: String(scenario.pypi),
        NPM_PUBLISH: String(scenario.npm),
    };
    function walk(
        wf,
        prefix = "",
        inputValues = {},
        active = true,
        outer = new Set(),
    ) {
        const inputs = Object.fromEntries(
            Object.entries(
                wf.on?.workflow_call?.inputs ||
                    wf.on?.workflow_dispatch?.inputs ||
                    {},
            ).map(([k, v]) => [k, v.default]),
        );
        Object.assign(inputs, inputValues);
        const outcomes = {};
        const closures = {};
        const pending = new Map(Object.entries(wf.jobs));
        while (pending.size) {
            let progress = false;
            for (const [id, job] of pending) {
                const deps =
                    typeof job.needs === "string"
                        ? [job.needs]
                        : job.needs || [];
                if (deps.some((d) => !outcomes[d])) continue;
                const needs = Object.fromEntries(
                    deps.map((d) => [d, outcomes[d]]),
                );
                closures[id] = new Set([
                    ...outer,
                    ...deps.flatMap((d) => [prefix + d, ...closures[d]]),
                ]);
                const ctx = {
                    github,
                    needs,
                    inputs,
                    vars,
                    secrets: {},
                    matrix: {},
                };
                let enabled =
                    job.if === undefined
                        ? true
                        : Boolean(expression(job.if, ctx));
                if (
                    !/(always|failure|cancelled|success)\s*\(/.test(
                        job.if || "",
                    ) &&
                    deps.some((d) => outcomes[d].result !== "success")
                )
                    enabled = false;
                enabled = active && enabled;
                let result = enabled ? "success" : "skipped";
                result = overrides[prefix + id] || result;
                outcomes[id] = { result, outputs: {} };
                if (id === "changes" && result === "success")
                    outcomes[id].outputs = {
                        rust: String(scenario.rust ?? true),
                        macos_required: String(classification.macosRequired),
                        tier: classification.isReleaseOnly
                            ? "sweep"
                            : "affected",
                    };
                // Both guards report a secret's presence; doubled at that boundary.
                if (id === "guard")
                    outcomes[id].outputs = {
                        publish: String(scenario.crate),
                        enabled: String(scenario.releaseToken ?? false),
                    };
                if (id === "arches")
                    outcomes[id].outputs = {
                        matrix: JSON.stringify(screencompMatrix(inputs)),
                    };
                // Resolve even skipped jobs: an unsupported runner must never count as zero.
                for (const matrix of matrixRows(job.strategy?.matrix, ctx)) {
                    const context = { ...ctx, matrix };
                    const name =
                        interpolate(job.name || id, context) +
                        (!job.name && Object.keys(matrix).length
                            ? ` (${Object.values(matrix)
                                  .filter((v) => v !== "")
                                  .join(", ")})`
                            : "");
                    if (job.uses) {
                        walk(
                            reusable(job.uses),
                            `${prefix}${name} / `,
                            Object.fromEntries(
                                Object.entries(job.with || {}).map(([k, v]) => [
                                    k,
                                    interpolate(v, context),
                                ]),
                            ),
                            enabled,
                            closures[id],
                        );
                    } else {
                        const runner = interpolate(job["runs-on"], context);
                        const labels = Array.isArray(runner)
                            ? runner
                            : [runner];
                        if (
                            labels.some(
                                (l) =>
                                    typeof l !== "string" ||
                                    !l ||
                                    l.includes("${{"),
                            ) ||
                            !labels.some((l) =>
                                /^(ubuntu|windows|macos)[-\w.]*$/.test(l),
                            )
                        )
                            throw new Error(
                                `unresolved runner ${file}:${id}: ${JSON.stringify(runner)}`,
                            );
                        const row = {
                            workflow: file,
                            event: scenario.event,
                            job: prefix + id,
                            name: prefix + name,
                            target:
                                matrix.target ||
                                matrix.arch ||
                                job.env?.DARWIN_TARGETS ||
                                null,
                            runner,
                            scheduled: enabled,
                            macos:
                                enabled &&
                                labels.some((l) => l.startsWith("macos")),
                            result,
                        };
                        // Step-level detail for effects and artifacts, kept out of telemetry.
                        Object.defineProperties(row, {
                            definition: { value: job },
                            context: { value: context },
                            closure: { value: closures[id] },
                        });
                        rows.push(row);
                    }
                }
                pending.delete(id);
                progress = true;
            }
            if (!progress)
                throw new Error(
                    `${file}: unresolved needs/cycle: ${[...pending.keys()]}`,
                );
        }
    }
    walk(
        workflow,
        "",
        scenario.event !== "workflow_dispatch"
            ? {}
            : scenario.inputs ||
                  (scenario.diagnostic !== undefined
                      ? { "macos-diagnostic": scenario.diagnostic }
                      : {}),
    );
    return rows;
}
export function resolveScenario(
    scenario,
    classification,
    sources = workflows(),
) {
    return Object.entries(sources)
        .filter(([file, wf]) => triggered(wf, scenario, file))
        .flatMap(([file, wf]) => {
            const rows = resolveRun(file, wf, scenario, classification);
            artifacts({ workflow: file, scenario, rows }, null);
            return rows;
        });
}

// ---- Artifacts: which job consumes what, and whether something made it first ----
const UPLOAD = /^actions\/upload-artifact@/;
const DOWNLOAD = /^actions\/download-artifact@/;
const stepsOf = (row) => row.definition?.steps || [];
function stepRuns(row, step) {
    if (step.if === undefined) return true;
    return Boolean(expression(step.if, { ...row.context, steps: {}, env: {} }));
}
function uploads(run) {
    return run.rows.flatMap((row) =>
        stepsOf(row)
            .filter((step) => UPLOAD.test(step.uses || ""))
            .map((step) => ({
                row,
                step,
                name: String(interpolate(step.with?.name ?? "artifact", row.context)),
            })),
    );
}
const artifactScope = (run) =>
    run.scenario.event === "release" || run.scenario.branch === "tag"
        ? "tag"
        : "exact-head";
// Every download is matched to the upload it reuses: in its own run through the
// consumer's `needs`, or in the run that triggered it through
// `run-id: github.event.workflow_run.id`. An artifact nothing upstream of the job
// made is one the job would have to build again, so it is added to the run as a
// rebuild row — on a Mac when it is a Darwin artifact, because this repository
// builds those only natively (`scripts/release-darwin.sh`).
export function artifacts(run, upstream) {
    const edges = [];
    const own = uploads(run);
    for (const row of [...run.rows]) {
        if (!row.scheduled) continue;
        for (const step of stepsOf(row)) {
            if (!DOWNLOAD.test(step.uses || "") || !stepRuns(row, step)) continue;
            const where = `${run.workflow}:${row.job}`;
            const runId = step.with?.["run-id"];
            if (
                runId !== undefined &&
                !/^\$\{\{\s*github\.event\.workflow_run\.id\s*\}\}$/.test(
                    String(runId),
                )
            )
                throw new Error(`${where}: unresolved artifact run-id ${runId}`);
            const source = runId === undefined ? run : upstream;
            if (!source)
                throw new Error(
                    `${where}: downloads from an upstream run that no workflow_run event supplied`,
                );
            const offered = source === run ? own : uploads(source);
            const name =
                step.with?.name === undefined
                    ? undefined
                    : String(interpolate(step.with.name, row.context));
            const pattern = name ?? step.with?.pattern;
            if (pattern === undefined)
                throw new Error(
                    `${where}: a download needs a name or pattern to be traced`,
                );
            const wanted =
                name !== undefined
                    ? [name]
                    : [
                          ...new Set(
                              offered
                                  .map((u) => u.name)
                                  .filter((n) => glob(pattern, n)),
                          ),
                      ];
            if (!wanted.length)
                throw new Error(
                    `${where}: artifact ${pattern} has no producer in ${source.workflow}`,
                );
            for (const artifact of wanted) {
                const producer = offered.find(
                    (u) =>
                        u.name === artifact &&
                        u.row.scheduled &&
                        stepRuns(u.row, u.step) &&
                        (source !== run || row.closure.has(u.row.job)),
                );
                edges.push({
                    consumer: `${run.workflow}:${row.name}`,
                    artifact,
                    scope: artifactScope(source),
                    producer: producer
                        ? `${source.workflow}:${producer.row.name}`
                        : null,
                });
                if (producer) continue;
                const darwin = /apple-darwin/.test(artifact);
                run.rows.push({
                    workflow: run.workflow,
                    event: run.scenario.event,
                    job: `${row.job} (rebuild ${artifact})`,
                    name: `${row.name} (rebuild ${artifact})`,
                    target:
                        artifact.match(
                            /[a-z0-9_]+-(?:apple-darwin|unknown-linux-gnu|pc-windows-msvc)/,
                        )?.[0] ?? null,
                    runner: darwin ? "macos-latest" : row.runner,
                    scheduled: true,
                    macos: darwin,
                    result: "success",
                    rebuild: true,
                });
            }
        }
    }
    return edges;
}

// ---- Lifecycle: the events one change sets off, read from the workflows ----
// What a step's command does on GitHub is the third-party boundary this model
// states rather than executes. Only these commands are read as starting further
// events; release-plz's own rules (tag a version that has no tag yet, otherwise
// open a release PR) come from its documentation and `release-plz.toml`.
const RELEASE = /\brelease-plz\s+release(?![-\w])/;
const RELEASE_PR = /\brelease-plz\s+release-pr\b/;
const AUTO_MERGE = /\bgh\s+pr\s+merge\b[^\n]*--auto/;
const DISPATCH = /\bgh\s+workflow\s+run\s+(\S+)([^\n]*)/;
function releasePlzConfig() {
    const read = `import json,tomllib;print(json.dumps(tomllib.load(open("release-plz.toml","rb")).get("workspace",{})))`;
    const workspace = JSON.parse(
        execFileSync("python3", ["-c", read], { encoding: "utf8" }),
    );
    return {
        // release-plz's documented defaults where the file names none.
        prefix: workspace.pr_branch_prefix ?? "release-plz-",
        title: workspace.pr_name ?? "chore: release",
        release: workspace.git_release_enable !== false,
        tag: (workspace.git_tag_name ?? "v{{ version }}").replace(
            "{{ version }}",
            "1.2.3",
        ),
    };
}
// Actions starts no workflow for an event made with the run's own token, except
// a dispatch; an effect whose token cannot be read is an error, never a guess.
function startsWorkflows(run, row, step) {
    const token = step.env?.GITHUB_TOKEN ?? step.env?.GH_TOKEN;
    if (token === undefined)
        throw new Error(
            `${run.workflow}:${row.job}: an effect step names no GITHUB_TOKEN/GH_TOKEN`,
        );
    return !/secrets\.GITHUB_TOKEN|github\.token/.test(String(token));
}
const carried = (ev) => ({
    pypi: ev.pypi,
    npm: ev.npm,
    crate: ev.crate,
    rust: ev.rust,
    releaseToken: ev.releaseToken,
});
function effects(run, ev, config) {
    const out = [];
    let released = false;
    for (const row of run.rows) {
        if (!row.scheduled || row.rebuild) continue;
        const opened = [];
        for (const step of stepsOf(row)) {
            if (typeof step.run !== "string" || !stepRuns(row, step)) continue;
            const cause = `${run.workflow}:${row.job}`;
            const pushed = ev.event === "push" && ev.branch !== "tag";
            // A pushed version bump has no tag yet, so release-plz tags it.
            if (RELEASE.test(step.run) && pushed && ev.diff === "version") {
                released = true;
                if (startsWorkflows(run, row, step)) {
                    const tagged = {
                        ...carried(ev),
                        branch: "tag",
                        tag: config.tag,
                        diff: ev.diff,
                        cause,
                    };
                    out.push({ ...tagged, event: "push", action: null });
                    if (config.release)
                        out.push({
                            ...tagged,
                            event: "release",
                            action: "published",
                        });
                }
            }
            if (RELEASE_PR.test(step.run) && pushed && !released) {
                if (startsWorkflows(run, row, step)) {
                    const pr = {
                        ...carried(ev),
                        event: "pull_request",
                        action: "opened",
                        branch: ev.branch,
                        headBranch: `${config.prefix}2026`,
                        fork: false,
                        diff: "version",
                        cause,
                    };
                    opened.push(pr);
                    out.push(pr);
                }
            }
            if (AUTO_MERGE.test(step.run))
                for (const pr of opened)
                    pr.merge = {
                        title: /--squash/.test(step.run)
                            ? config.title
                            : `Merge pull request from ${pr.headBranch}`,
                    };
            const dispatch = step.run.match(DISPATCH);
            if (dispatch) {
                const inputs = Object.fromEntries(
                    [...dispatch[2].matchAll(/-[fF]\s+([\w-]+)=(\S+)/g)].map(
                        ([, key, value]) => [
                            key,
                            value === "true" ? true : value === "false" ? false : value,
                        ],
                    ),
                );
                const target = dispatch[1].replace(/^\.github\/workflows\//, "");
                out.push({
                    ...carried(ev),
                    event: "workflow_dispatch",
                    action: null,
                    branch: "main",
                    target,
                    inputs,
                    cause,
                });
            }
        }
    }
    return out;
}
function label(ev, config) {
    if (ev.event === "pull_request") return `pr-${ev.action}`;
    if (ev.event === "push")
        return ev.branch === "tag"
            ? "release-tag"
            : ev.headBranch?.startsWith(config.prefix) && !ev.fork
              ? "release-pr-merge"
              : "base-push";
    if (ev.event === "release") return "release-published";
    if (ev.event === "workflow_run") return "downstream";
    if (ev.event === "workflow_dispatch")
        return ev.inputs?.["macos-diagnostic"] === true
            ? "manual-opt-in"
            : "manual-default";
    if (ev.event === "repository_dispatch") return "repository-dispatch";
    if (ev.event === "schedule") return "scheduled";
    return ev.event;
}
// Follow one change from its root event through every run the workflows start:
// triggers and filters decide which workflows an event starts, job conditions
// and needs decide which jobs run, step commands decide which further events
// they make, a merge becomes a base push, a completed run becomes a
// `workflow_run` for every workflow subscribed to it, and every artifact a job
// downloads is traced to the run that made it.
export function lifecycle(root, { classify, sources = workflows(), limit = 64 }) {
    const config = releasePlzConfig();
    const subscribed = new Set(
        Object.values(sources).flatMap((wf) =>
            typeof wf.on === "object" && !Array.isArray(wf.on)
                ? wf.on.workflow_run?.workflows || []
                : [],
        ),
    );
    const events = [];
    const runs = [];
    const queue = [{ ...root, cause: root.cause ?? "root" }];
    while (queue.length) {
        if (events.length >= limit)
            throw new Error(`lifecycle did not settle within ${limit} events`);
        const ev = queue.shift();
        const index = events.length;
        events.push({
            index,
            label: label(ev, config),
            event: ev.event,
            action: ev.action ?? null,
            head: ev.headBranch ?? null,
            cause: ev.cause,
        });
        const classification = classify(ev);
        for (const [file, wf] of Object.entries(sources)) {
            if (!triggered(wf, ev, file)) continue;
            const run = {
                index,
                workflow: file,
                scenario: ev,
                rows: resolveRun(file, wf, ev, classification),
            };
            run.artifacts = artifacts(run, ev.upstreamRun ?? null);
            runs.push(run);
            queue.push(...effects(run, ev, config));
            const name = wf.name ?? `.github/workflows/${file}`;
            if (subscribed.has(name))
                queue.push({
                    ...carried(ev),
                    event: "workflow_run",
                    action: "completed",
                    branch: "main",
                    upstream: name,
                    upstreamRun: run,
                    cause: `${file} completed`,
                });
        }
        // A merged pull request lands on its base as a push and closes.
        if (ev.event === "pull_request" && ev.merge) {
            const merged = {
                ...carried(ev),
                diff: ev.diff,
                fork: ev.fork,
                headBranch: ev.headBranch,
                branch: ev.branch,
                cause: `merge of ${ev.headBranch}`,
            };
            queue.push({
                ...merged,
                event: "push",
                action: null,
                headCommit: ev.merge.title,
            });
            queue.push({ ...merged, event: "pull_request", action: "closed" });
        }
    }
    const spending = runs
        .filter((run) => run.rows.some((row) => row.macos))
        .map((run) => ({
            event: events[run.index].label,
            index: run.index,
            workflow: run.workflow,
            jobs: run.rows.filter((row) => row.macos),
        }));
    return {
        events,
        workflow_runs: runs.map((run) => ({
            index: run.index,
            event: events[run.index].label,
            workflow: run.workflow,
            mac_jobs: run.rows.filter((row) => row.macos).length,
            artifacts: run.artifacts,
        })),
        spending,
        runs: spending.length,
    };
}
