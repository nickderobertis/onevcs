import test from "node:test";
import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { resolve, join } from "node:path";
import { NON_PR_EVENTS } from "../macos-policy.mjs";
const root = resolve(".");
export function history() {
    const scratch =
        process.env.ONEPIPELINE_NODE_SCRATCH_DIR ||
        resolve(".github/.telemetry");
    mkdirSync(scratch, { recursive: true });
    const dir = mkdtempSync(join(scratch, "macos-history-"));
    const git = (...args) =>
        execFileSync("git", args, { cwd: dir, encoding: "utf8" }).trim();
    git("init", "-q");
    git("config", "user.email", "test@example.invalid");
    git("config", "user.name", "Policy test");
    mkdirSync(join(dir, "crates/onevcs"), { recursive: true });
    const manifest =
        '[package]\nname = "onevcs"\nversion = "0.1.0"\n[dependencies]\nserde = "1"\n';
    writeFileSync(join(dir, "crates/onevcs/Cargo.toml"), manifest);
    const lock =
        'version = 4\n[[package]]\nname = "onevcs"\nversion = "0.1.0"\n[[package]]\nname = "onevcs"\nversion = "1.0.0"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "registry-checksum"\n';
    writeFileSync(join(dir, "Cargo.lock"), lock);
    writeFileSync(join(dir, "CHANGELOG.md"), "# Changes\n");
    writeFileSync(join(dir, "source.rs"), "fn main() {}\n");
    git("add", ".");
    git("commit", "-qm", "base");
    const base = git("rev-parse", "HEAD");
    const refs = {};
    for (const kind of ["source", "version", "dependency", "changelog", "registry-dependency"]) {
        git("checkout", "-q", "--detach", base);
        writeFileSync(
            join(dir, "crates/onevcs/Cargo.toml"),
            kind === "version"
                ? manifest.replace("0.1.0", "0.2.0")
                : kind === "dependency"
                  ? manifest.replace('serde = "1"', 'serde = "2"')
                  : manifest,
        );
        if (kind === "registry-dependency")
            writeFileSync(
                join(dir, "Cargo.lock"),
                lock.replace('version = "1.0.0"', 'version = "2.0.0"'),
            );
        if (kind === "source")
            writeFileSync(
                join(dir, "source.rs"),
                'fn main() { println!("change"); }\n',
            );
        if (kind === "changelog")
            writeFileSync(
                join(dir, "CHANGELOG.md"),
                "# Changes\n\n## 0.2.0\n- Updated release notes.\n",
            );
        git("add", ".");
        git("commit", "-qm", kind);
        refs[kind] = git("rev-parse", "HEAD");
    }
    const classify = (
        kind,
        {
            fork = false,
            branch = "release-plz-2026",
            author = "nickderobertis",
            title = "chore: release",
            malformed = false,
            malformedData = false,
            head = refs[kind],
            eventName = "pull_request",
        } = {},
    ) => {
        const event = {
            repository: { full_name: "nickderobertis/onevcs" },
            pull_request: {
                title,
                user: { login: author },
                head: {
                    ref: branch,
                    repo: {
                        full_name: fork
                            ? "someone/fork"
                            : "nickderobertis/onevcs",
                    },
                },
            },
        };
        if (malformedData) {
            event.repository.full_name = 1;
            event.pull_request.head.repo.full_name = 1;
            event.pull_request.head.ref = ["release-plz-forged"];
        }
        const path = join(dir, "event.json");
        writeFileSync(path, malformed ? "{" : JSON.stringify(event));
        return JSON.parse(
            execFileSync(
                "node",
                [
                    join(root, ".github/macos-policy.mjs"),
                    "classify",
                    "--event",
                    path,
                    "--base",
                    base,
                    "--head",
                    head,
                ],
                {
                    cwd: dir,
                    encoding: "utf8",
                    env: { ...process.env, GITHUB_EVENT_NAME: eventName },
                },
            ),
        );
    };
    return {
        dir,
        base,
        refs,
        classify,
        cleanup: () => rmSync(dir, { recursive: true, force: true }),
    };
}
test("real git semantic classifier admits only same-repo release versions", () => {
    const h = history();
    try {
        assert.equal(h.classify("version").isReleaseOnly, true);
        assert.equal(h.classify("changelog").isReleaseOnly, true);
        assert.equal(h.classify("changelog").macosRequired, false);
        assert.equal(h.classify("changelog", { fork: true }).macosRequired, true);
        assert.equal(h.classify("changelog", { branch: "feature" }).macosRequired, true);
        assert.equal(
            h.classify("version", { malformedData: true }).macosRequired,
            true,
        );
        assert.equal(
            h.classify("version", { author: "github-actions[bot]" })
                .isReleaseOnly,
            true,
        );
        for (const options of [
            { branch: "feature", author: "github-actions[bot]" },
            { branch: "feature", title: "chore: release" },
            { fork: true },
            { malformed: true },
            { head: "missing" },
        ])
            assert.equal(h.classify("version", options).macosRequired, true);
        for (const kind of ["source", "dependency", "registry-dependency"])
            assert.equal(h.classify(kind).macosRequired, true);
        assert.equal(
            h.classify("source", { eventName: "push", malformed: true })
                .macosRequired,
            false,
        );
        // Every known non-PR automatic event stays Mac-free, whatever the diff,
        // branch or ownership; an event nobody decided about fails closed.
        for (const eventName of NON_PR_EVENTS)
            for (const kind of ["source", "version", "dependency"])
                for (const fork of [false, true]) {
                    const answer = h.classify(kind, {
                        eventName,
                        fork,
                        branch: "release-plz-2026",
                    });
                    assert.equal(answer.macosRequired, false, eventName);
                    assert.equal(answer.isReleaseOnly, false, eventName);
                    assert.match(answer.reason, new RegExp(eventName));
                }
        for (const eventName of ["merge_group", "pull_request_target"]) {
            const answer = h.classify("version", {
                eventName,
                branch: "release-plz-2026",
            });
            assert.equal(answer.macosRequired, true, eventName);
            assert.equal(answer.isReleaseOnly, false, eventName);
            assert.match(answer.reason, new RegExp(`unrecognised event "${eventName}"`));
        }
    } finally {
        h.cleanup();
    }
});
test("carriers drive production command over GitHub success/failure/cancel/skip/missing outcomes", () => {
    for (const lane of ["cross", "install"]) {
        const invoke = (needs) =>
            spawnSync(
                "node",
                [".github/required-result.mjs", lane, JSON.stringify(needs)],
                { encoding: "utf8" },
            );
        const needs = {
            changes: {
                result: "success",
                outputs: { rust: "true", macos_required: "true" },
            },
            "macos-pr": { result: "success" },
            gate: { result: "success" },
        };
        assert.equal(invoke(needs).status, 0);
        for (const result of ["failure", "cancelled", "skipped", undefined]) {
            assert.notEqual(
                invoke({ ...needs, "macos-pr": { result } }).status,
                0,
            );
            assert.notEqual(
                invoke({ ...needs, changes: { ...needs.changes, result } })
                    .status,
                0,
            );
            assert.notEqual(invoke({ ...needs, gate: { result } }).status, 0);
        }
        const omitted = {
            ...needs,
            changes: {
                result: "success",
                outputs: { rust: "true", macos_required: "false" },
            },
            "macos-pr": { result: "skipped" },
        };
        const result = invoke(omitted);
        assert.equal(result.status, 0);
        assert.match(result.stdout, /omitted/);
        assert.doesNotMatch(result.stdout, /passed/);
        assert.match(result.stdout, /Mac classifier authorizes omission/);
        const pathOmitted = invoke({
            ...omitted,
            changes: {
                result: "success",
                outputs: { rust: "false", macos_required: "true" },
            },
        });
        assert.equal(pathOmitted.status, 0);
        assert.match(pathOmitted.stdout, /Rust path filter reports unaffected/);
        assert.doesNotMatch(pathOmitted.stdout, /passed/);
        for (const malformed of [
            null,
            [],
            {},
            { changes: { result: "success" } },
        ])
            assert.notEqual(invoke(malformed).status, 0);
        assert.notEqual(
            invoke({ ...omitted, "macos-pr": { result: "failure" } }).status,
            0,
        );
    }
});
