import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { workflows } from "../workflow-graph.mjs";
const read = (p) => readFileSync(p, "utf8");
test("actual release routes retain every platform and package, Darwin needs precede publish", () => {
    const wf = workflows()["release.yml"];
    const platforms = Object.keys(
        JSON.parse(read("npm/onevcs/package.json")).optionalDependencies,
    );
    const nonMac = wf.jobs["build-npm"].strategy.matrix.include.map(
        (r) => r.target,
    );
    const macArtifacts = wf.jobs["build-darwin"].steps
        .filter((s) => s.uses === "actions/upload-artifact@v4")
        .map((s) => s.with);
    const macTargets = [
        ...new Set(
            macArtifacts
                .filter((s) => s.name.startsWith("npm-"))
                .map((s) => s.name.slice(4)),
        ),
    ];
    assert.equal(nonMac.length + macTargets.length, platforms.length);
    for (const target of nonMac)
        for (const job of ["upload", "build-wheels"])
            assert.equal(
                wf.jobs[job].strategy.matrix.include.filter(
                    (r) => r.target === target,
                ).length,
                1,
            );
    const targetScript = read("scripts/release-darwin.sh");
    for (const target of macTargets) {
        assert.ok(
            wf.jobs["build-darwin"].env.DARWIN_TARGETS.split(" ").includes(
                target,
            ),
        );
        for (const form of ["archives", "wheels", "npm"])
            assert.equal(
                macArtifacts.filter((a) => a.name === `${form}-${target}`)
                    .length,
                1,
            );
    }
    for (const job of ["publish-pypi", "publish-npm"])
        assert.ok(wf.jobs[job].needs.includes("build-darwin"));
    assert.equal(wf.jobs["upload-darwin"]["runs-on"], "ubuntu-latest");
    assert.equal(wf.jobs["upload-darwin"].needs, "build-darwin");
    assert.match(
        targetScript,
        /release-stage.sh "\$target"[\s\S]*release-local-install.sh "\$target"/,
    );
    const stage = read("scripts/release-stage.sh");
    assert.equal(stage.match(/^maturin build/gm).length, 1);
    assert.doesNotMatch(targetScript, /cargo build/);
    assert.match(
        stage,
        /maturin build --release --locked --target "\$target" --strip false/,
    );
    assert.match(
        stage,
        /npm-build.mjs platform --target "\$target" --binary "\$bin"/,
    );
    assert.match(stage, /release-payload-check.py/);
    assert.match(stage, /release-wheel-binary.py/);
    const install = read("scripts/release-local-install.sh");
    // Darwin execution requires the real Mac runner; local proof checks the
    // commands and foreign payload contracts rather than executing Darwin on Linux.
    // Only Apple Silicon is built, natively: no Rosetta, no x86_64 cross-compile.
    assert.deepEqual(
        wf.jobs["build-darwin"].env.DARWIN_TARGETS.split(" "),
        ["aarch64-apple-darwin"],
    );
    assert.doesNotMatch(
        JSON.stringify(wf.jobs["build-darwin"]),
        /rosetta|x86_64|architecture/i,
    );
    assert.doesNotMatch(install, /arch -x86_64|NODE_X64/);
    assert.ok(!platforms.some((p) => p.includes("darwin-x64")));
    assert.match(install, /pip install --no-index --no-deps/);
    assert.match(install, /npm install .*--offline/);
    assert.equal(install.match(/bash scripts\/smoke-published.sh/g).length, 2);
    for (const a of macArtifacts) assert.equal(a["if-no-files-found"], "error");
    // download-artifact defaults to this run, checkout defaults to its release tag.
    for (const job of ["publish-pypi", "publish-npm", "upload-darwin"])
        for (const step of wf.jobs[job].steps.filter(
            (s) => s.uses === "actions/download-artifact@v4",
        ))
            assert.equal(step.with["run-id"], undefined);
});

test("CI declares its read-only default at workflow scope", () => {
    const ci = workflows()["ci.yml"];
    assert.deepEqual(ci.permissions, { contents: "read" });
    assert.equal(ci.on.pull_request, null);
});
