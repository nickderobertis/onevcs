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
function triggered(workflow, scenario) {
    const on = workflow.on;
    if (typeof on === "string") return on === scenario.event;
    if (Array.isArray(on)) return on.includes(scenario.event);
    if (!on || !(scenario.event in on)) return false;
    const spec = on[scenario.event];
    // Actions' default pull_request activity set excludes close and draft lift.
    if (
        scenario.event === "pull_request" &&
        !spec?.types &&
        !["opened", "synchronize", "reopened"].includes(scenario.action)
    )
        return false;
    if (spec?.types && !spec.types.includes(scenario.action)) return false;
    if (spec?.branches && !spec.branches.includes(scenario.branch))
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
            pull_request: {
                head: {
                    repo: {
                        full_name: scenario.fork
                            ? "someone/fork"
                            : "nickderobertis/onevcs",
                    },
                },
            },
            workflow_run: { conclusion: "success" },
        },
        repository: "nickderobertis/onevcs",
        ref: scenario.branch === "tag" ? "refs/tags/v1.2.3" : "refs/heads/main",
        ref_name: "v1.2.3",
    };
    const vars = {
        PYPI_PUBLISH: String(scenario.pypi),
        NPM_PUBLISH: String(scenario.npm),
    };
    function walk(wf, prefix = "", inputValues = {}, active = true) {
        const inputs = Object.fromEntries(
            Object.entries(
                wf.on?.workflow_call?.inputs ||
                    wf.on?.workflow_dispatch?.inputs ||
                    {},
            ).map(([k, v]) => [k, v.default]),
        );
        Object.assign(inputs, inputValues);
        const outcomes = {};
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
                if (id === "guard")
                    outcomes[id].outputs = { publish: String(scenario.crate) };
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
                        rows.push({
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
                        });
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
        scenario.event === "workflow_dispatch" &&
            scenario.diagnostic !== undefined
            ? { "macos-diagnostic": scenario.diagnostic }
            : {},
    );
    return rows;
}
export function resolveScenario(scenario, classification) {
    return Object.entries(workflows())
        .filter(([, wf]) => triggered(wf, scenario))
        .flatMap(([file, wf]) =>
            resolveRun(file, wf, scenario, classification),
        );
}
