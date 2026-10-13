import test from "node:test";
import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
function compare(path, before, after) {
    return (
        execFileSync("python3", [".github/version-only.py", path], {
            input: JSON.stringify({ before, after }),
            encoding: "utf8",
            stdio: ["pipe", "pipe", "pipe"],
        }).trim() === "true"
    );
}
test("real semantic boundary admits package/lock versions and rejects other fields", () => {
    const manifest = readFileSync("crates/onevcs-testing/Cargo.toml", "utf8");
    const bumpedPackage = manifest.replace(
        /^version = "[^"]+"/m,
        'version = "9.8.7"',
    );
    assert.notEqual(
        bumpedPackage,
        manifest,
        "the fixture must actually change the package version",
    );
    assert.equal(
        compare("crates/onevcs-testing/Cargo.toml", manifest, bumpedPackage),
        true,
    );
    assert.equal(
        compare(
            "crates/onevcs-testing/Cargo.toml",
            manifest,
            manifest.replace(/version = "=[^"]+"/, 'version = "=9.8.7"'),
        ),
        true,
    );
    assert.equal(
        compare(
            "crates/onevcs-testing/Cargo.toml",
            manifest,
            manifest.replace('features = ["testing"]', "features = []"),
        ),
        false,
    );
    assert.equal(
        compare(
            "crates/onevcs-testing/Cargo.toml",
            manifest,
            manifest.replace('version = "=0.49.0"', 'version = "^0.49.0"'),
        ),
        false,
    );
    const lock =
        'version = 4\n[[package]]\nname = "onevcs"\nversion = "0.1.0"\n[[package]]\nname = "consumer"\nversion = "1.0.0"\ndependencies = ["onevcs 0.1.0"]\n';
    assert.equal(
        compare("compat/Cargo.lock", lock, lock.replaceAll("0.1.0", "0.2.0")),
        true,
    );
    assert.equal(
        compare("compat/Cargo.lock", lock, lock.replace("1.0.0", "2.0.0")),
        false,
    );
    assert.equal(compare("Cargo.lock", lock, lock.replace("0.1.0", "garbage")), false);
    assert.throws(() => compare("Cargo.lock", lock, "not TOML"));
});

test("semantic CLI rejects malformed invocation/input separately from a valid negative answer", () => {
    for (const [args, input] of [[[], "{}"], [["unsupported-path"], JSON.stringify({before: '[package]\nname="onevcs"\nversion="0.1.0"', after: '[package]\nname="onevcs"\nversion="0.2.0"'})], [["Cargo.lock"], "{"], [["Cargo.lock"], "null"], [["Cargo.lock"], JSON.stringify({before: 1, after: []})]]) {
        const run = spawnSync("python3", [".github/version-only.py", ...args], {input, encoding: "utf8"});
        assert.notEqual(run.status, 0);
        assert.equal(run.stdout, "");
        assert.match(run.stderr, /ACTION:/);
    }
    assert.equal(compare("Cargo.lock", "version = 4", "version = 4"), false);
});
