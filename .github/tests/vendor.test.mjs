import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, copyFileSync, existsSync, rmSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";

test("actual reusable workflow loading verifies the pinned URL and raw-source digest", () => {
    const base = process.env.ONEPIPELINE_NODE_SCRATCH_DIR || resolve(".github/.telemetry");
    mkdirSync(base, { recursive: true });
    const dir = mkdtempSync(join(base, "vendor-proof-"));
    try {
        mkdirSync(join(dir, ".github/vendor"), { recursive: true });
        copyFileSync("screencomp.toml", join(dir, "screencomp.toml"));
        const inventory = JSON.parse(readFileSync(".github/vendor/workflow-sources.json", "utf8"));
        const call = Object.keys(inventory)[0];
        const source = join(dir, ".github/vendor/workflow-sources.json");
        const command = `import { resolveRun } from ${JSON.stringify(pathToFileURL(resolve(".github/workflow-graph.mjs")).href)}; resolveRun("caller", {jobs:{call:{uses:${JSON.stringify(call)}}}}, {event:"pull_request",pypi:true,npm:true}, {macosRequired:true});`;
        const run = () => spawnSync("node", ["--input-type=module", "-e", command], {cwd: dir, encoding: "utf8"});
        for (const field of ["url", "source", "sha256"]) {
            const broken = structuredClone(inventory);
            broken[call][field] += "corruption";
            writeFileSync(source, JSON.stringify(broken));
            const invalid = run();
            assert.notEqual(invalid.status, 0);
            assert.match(invalid.stderr, field === "url" ? /source URL mismatch/ : /source SHA-256 mismatch/);
        }
        writeFileSync(source, JSON.stringify(inventory));
        const recovered = run();
        assert.equal(recovered.status, 0, recovered.stderr);
    } finally {
        rmSync(dir, {recursive: true, force: true});
    }
});

test("real lint command reports syntax failures with recovery instructions", () => {
    const base = process.env.ONEPIPELINE_NODE_SCRATCH_DIR || resolve(".github/.telemetry");
    mkdirSync(base, { recursive: true });
    const dir = mkdtempSync(join(base, "syntax-proof-"));
    try {
        for (const path of [".github/tests", ".github/workflows", ".github/bin"])
            mkdirSync(join(dir, path), {recursive: true});
        copyFileSync(".github/check-syntax.mjs", join(dir, ".github/check-syntax.mjs"));
        const tool = existsSync(".github/bin/actionlint.exe") ? "actionlint.exe" : "actionlint";
        copyFileSync(resolve(".github/bin", tool), join(dir, ".github/bin", tool));
        writeFileSync(join(dir, ".github/workflows/ci.yml"), "name: Fixture\non: push\njobs:\n  check:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo verified\n");
        const module = join(dir, ".github/fixture.mjs");
        writeFileSync(module, "export const broken = ;");
        const run = () => spawnSync("bash", [resolve(".github/lint.sh")], {cwd: dir, encoding: "utf8"});
        const invalid = run();
        assert.notEqual(invalid.status, 0);
        assert.match(invalid.stderr, /fixture.mjs/);
        assert.match(invalid.stderr, /ACTION: fix the named file/);
        writeFileSync(module, "export const repaired = true;");
        const recovered = run();
        assert.equal(recovered.status, 0, recovered.stderr);
    } finally {
        rmSync(dir, {recursive: true, force: true});
    }
});
