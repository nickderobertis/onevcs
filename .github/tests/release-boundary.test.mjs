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
