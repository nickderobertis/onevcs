import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
test("release helper boundaries reject malformed input before building or installing", () => {
    for (const [command, args] of [
        ["node", [".github/required-result.mjs", "unknown", "{}"]],
        ["python3", ["scripts/release-archive.py"]],
        [
            "python3",
            ["scripts/release-archive.py", "../../escape", "absent", "absent"],
        ],
        ["python3", ["scripts/release-payload-check.py"]],
        [
            "python3",
            [
                "scripts/release-payload-check.py",
                "unreleased-target",
                "absent",
                "absent",
                "bad-version",
            ],
        ],
        ["bash", ["scripts/release-stage.sh", "unreleased-target"]],
        ["bash", ["scripts/release-local-install.sh", "unreleased-target"]],
    ]) {
        const result = spawnSync(command, args, { encoding: "utf8" });
        assert.notEqual(
            result.status,
            0,
            `${command} ${args} admitted invalid input`,
        );
        assert.match(result.stderr, /ACTION:/);
    }
});

test("Darwin build reports registry/toolchain transport failures before compilation", async () => {
    const { mkdtempSync, mkdirSync, writeFileSync, rmSync } = await import(
        "node:fs"
    );
    const { resolve, join, delimiter } = await import("node:path");
    const { workflows } = await import("../workflow-graph.mjs");
    const python = spawnSync("bash", ["-c", "command -v python3"], {encoding: "utf8"}).stdout.trim();
    const base =
        process.env.ONEPIPELINE_NODE_SCRATCH_DIR ||
        resolve(".github/.telemetry");
    mkdirSync(base, { recursive: true });
    const dir = mkdtempSync(join(base, "darwin-transport-"));
    try {
        // Only installer transport results are doubled; the release shell runs unchanged.
        writeFileSync(
            join(dir, "python3"),
            '#!/bin/bash\n[ "$*" = "-m pip install maturin==1.14.1" ] || exec "$REAL_PYTHON" "$@"\nexit "$PIP_TRANSPORT_RESULT"\n',
            { mode: 0o755 },
        );
        writeFileSync(
            join(dir, "rustup"),
            '#!/bin/bash\n[ "$1" = target ] && [ "$2" = add ] || exit 2\nexit 1\n',
            { mode: 0o755 },
        );
        for (const targets of ["--help", "x86_64-unknown-linux-gnu", "x86_64-apple-darwin x86_64-apple-darwin"]) {
            const invalid = spawnSync("bash", ["scripts/release-darwin.sh"], {env: {...process.env, PATH: dir + delimiter + process.env.PATH, DARWIN_TARGETS: targets, REAL_PYTHON: python, PIP_TRANSPORT_RESULT: "1"}, encoding: "utf8"});
            assert.notEqual(invalid.status, 0);
            assert.match(invalid.stderr, /unsupported release target|non-Darwin target|duplicate target/);
            assert.match(invalid.stderr, /ACTION:/);
            assert.doesNotMatch(invalid.stderr, /maturin install failed|target installation failed/);
        }
        for (const result of ["1", "0"]) {
            const run = spawnSync("bash", ["scripts/release-darwin.sh"], {
                env: {
                    ...process.env,
                    PATH: dir + delimiter + process.env.PATH,
                    DARWIN_TARGETS:
                        workflows()["release.yml"].jobs["build-darwin"].env
                            .DARWIN_TARGETS,
                    PIP_TRANSPORT_RESULT: result,
                    REAL_PYTHON: python,
                },
                encoding: "utf8",
            });
            assert.notEqual(run.status, 0);
            assert.match(
                run.stderr,
                result === "1"
                    ? /maturin install failed/
                    : /target installation failed/,
            );
            assert.match(run.stderr, /ACTION:/);
        }
    } finally {
        rmSync(dir, { recursive: true, force: true });
    }
});
