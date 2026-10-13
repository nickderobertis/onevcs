import test from "node:test";
import assert from "node:assert/strict";
import {
    mkdtempSync,
    mkdirSync,
    writeFileSync,
    readFileSync,
    copyFileSync,
    existsSync,
    rmSync,
} from "node:fs";
import { execFileSync, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { resolve, join, delimiter } from "node:path";
function fixture(system, os, machine = "x86_64") {
    const architecture = ["arm64", "aarch64"].includes(machine)
        ? "arm64"
        : "amd64";
    const base =
        process.env.ONEPIPELINE_NODE_SCRATCH_DIR ||
        resolve(".github/.telemetry");
    mkdirSync(base, { recursive: true });
    const dir = mkdtempSync(join(base, "bootstrap-"));
    for (const path of [".github", "transport", "archive"])
        mkdirSync(join(dir, path));
    for (const file of ["bootstrap.sh", "package.json", "package-lock.json"])
        copyFileSync(`.github/${file}`, join(dir, ".github", file));
    const version = readFileSync(".github/bootstrap.sh", "utf8").match(
        /^version=(.*)$/m,
    )[1];
    const asset = readFileSync(".github/actionlint-checksums.txt", "utf8")
        .split("\n")
        .map((line) => line.split(/\s+/)[1])
        .find((name) => name?.startsWith(`actionlint_${version}_${os}_${architecture}.`));
    assert.ok(
        asset,
        "archive name must come from the production checksum authority",
    );
    const binary = os === "windows" ? "actionlint.exe" : "actionlint";
    writeFileSync(
        join(dir, "archive", binary),
        "#!/bin/sh\n# transport fixture; no foreign executable is run\n",
        { mode: 0o755 },
    );
    const response = join(dir, "response");
    if (asset.endsWith(".zip"))
        execFileSync("python3", [
            "-c",
            "import zipfile,sys;z=zipfile.ZipFile(sys.argv[1],'w');z.write(sys.argv[2],sys.argv[3]);z.close()",
            response,
            join(dir, "archive", binary),
            binary,
        ]);
    else
        execFileSync("tar", [
            "-czf",
            response,
            "-C",
            join(dir, "archive"),
            binary,
        ]);
    const original = readFileSync(response);
    const checksum = (bytes) =>
        `${createHash("sha256").update(bytes).digest("hex")}  ${asset}\n`;
    writeFileSync(
        join(dir, ".github/actionlint-checksums.txt"),
        checksum(original),
    );
    writeFileSync(
        join(dir, "transport/uname"),
        '#!/bin/bash\ncase "$1" in -s) echo "$BOOTSTRAP_SYSTEM";; -m) echo "$BOOTSTRAP_MACHINE";; *) exit 1;; esac\n',
        { mode: 0o755 },
    );
    writeFileSync(
        join(dir, "transport/curl"),
        '#!/bin/bash\nset -eu\nif [ "${BOOTSTRAP_FAIL_CURL:-0}" = 1 ]; then echo "injected download failure" >&2; exit 22; fi\nurl=""\noutput=""\nwhile [ "$#" -gt 0 ]; do\n case "$1" in https:*) url="$1";; -o) shift; output="$1";; esac\n shift\ndone\nprintf "%s\\n" "$url" >> "$BOOTSTRAP_REQUESTS"\ncp "$BOOTSTRAP_RESPONSE" "$output"\n',
        { mode: 0o755 },
    );
    const env = {
        ...process.env,
        PATH: join(dir, "transport") + delimiter + process.env.PATH,
        npm_config_offline: "true",
        BOOTSTRAP_SYSTEM: system,
        BOOTSTRAP_MACHINE: machine,
        BOOTSTRAP_RESPONSE: response,
        BOOTSTRAP_REQUESTS: join(dir, "requests"),
    };
    return {
        dir,
        asset,
        binary,
        response,
        original,
        checksum,
        run: (extra) =>
            spawnSync("bash", [".github/bootstrap.sh"], {
                cwd: dir,
                env: { ...env, ...extra },
                encoding: "utf8",
            }),
        reset: () => {
            const bin = join(dir, ".github/bin", binary);
            if (existsSync(bin)) rmSync(bin);
            const archive = join(dir, ".github/bin", asset);
            if (existsSync(archive)) rmSync(archive);
        },
        cleanup: () => rmSync(dir, { recursive: true, force: true }),
    };
}
test("real bootstrap uses declared zip/tar archives and rejects transport, integrity, host and extraction failures", () => {
    for (const [system, os] of [
        ["MINGW64_NT-10.0", "windows"],
        ["Linux", "linux"],
        ["Darwin", "darwin"],
    ]) {
        const f = fixture(system, os);
        try {
            const good = f.run();
            assert.equal(good.status, 0, good.stderr);
            assert.ok(existsSync(join(f.dir, ".github/bin", f.binary)));
            assert.ok(
                readFileSync(join(f.dir, "requests"), "utf8").includes(f.asset),
            );
            const requests = readFileSync(join(f.dir, "requests"), "utf8");
            const cached = join(f.dir, ".github/bin", f.binary);
            const expectedBinary = readFileSync(cached);
            writeFileSync(cached, "stale or corrupt cached executable");
            assert.equal(f.run().status, 0, "cache hit re-extracts only the verified archive");
            assert.deepEqual(readFileSync(cached), expectedBinary);
            assert.equal(readFileSync(join(f.dir, "requests"), "utf8"), requests);
            const cachedArchive = join(f.dir, ".github/bin", f.asset);
            const verifiedArchive = readFileSync(cachedArchive);
            writeFileSync(cachedArchive, "corrupt cached archive");
            const invalidCache = f.run();
            assert.notEqual(invalidCache.status, 0);
            assert.match(invalidCache.stderr, /checksum mismatch[\s\S]*ACTION:/);
            writeFileSync(cachedArchive, verifiedArchive);
            assert.equal(f.run().status, 0);
            f.reset();
            const unsupported = f.run({ BOOTSTRAP_SYSTEM: "unknown-host" });
            assert.notEqual(unsupported.status, 0);
            assert.match(unsupported.stderr, /unsupported host[\s\S]*ACTION:/);
            const arch = f.run({ BOOTSTRAP_MACHINE: "unknown-arch" });
            assert.notEqual(arch.status, 0);
            assert.match(arch.stderr, /unsupported architecture[\s\S]*ACTION:/);
            const download = f.run({ BOOTSTRAP_FAIL_CURL: "1" });
            assert.notEqual(download.status, 0);
            assert.match(download.stderr, /download failed[\s\S]*ACTION:/);
            writeFileSync(f.response, "corrupt response");
            const bad = f.run();
            assert.notEqual(bad.status, 0);
            assert.match(bad.stderr, /checksum mismatch[\s\S]*ACTION:/);
            f.reset();
            writeFileSync(f.response, f.original);
            writeFileSync(
                join(f.dir, ".github/actionlint-checksums.txt"),
                "invalid checksum row extra\n",
            );
            const malformed = f.run();
            assert.notEqual(malformed.status, 0);
            assert.match(
                malformed.stderr,
                /invalid checksum row[\s\S]*ACTION:/,
            );
            f.reset();
            writeFileSync(f.response, "not an archive");
            writeFileSync(
                join(f.dir, ".github/actionlint-checksums.txt"),
                f.checksum(Buffer.from("not an archive")),
            );
            const extraction = f.run();
            assert.notEqual(extraction.status, 0);
            assert.match(extraction.stderr, /extraction failed[\s\S]*ACTION:/);
            f.reset();
            writeFileSync(f.response, f.original);
            writeFileSync(
                join(f.dir, ".github/actionlint-checksums.txt"),
                f.checksum(f.original),
            );
            assert.equal(
                f.run().status,
                0,
                "recovery after all download boundary failures",
            );
        } finally {
            f.cleanup();
        }
    }
});

test("ARM host aliases download the declared ARM assets and recover after corruption", () => {
    for (const [system, os, machine] of [
        ["Linux", "linux", "aarch64"],
        ["Darwin", "darwin", "arm64"],
    ]) {
        const f = fixture(system, os, machine);
        try {
            const good = f.run();
            assert.equal(good.status, 0, good.stderr);
            assert.match(f.asset, /_arm64\.tar\.gz$/);
            assert.ok(
                readFileSync(join(f.dir, "requests"), "utf8").includes(f.asset),
            );
            f.reset();
            writeFileSync(f.response, "corrupt ARM response");
            const bad = f.run();
            assert.notEqual(bad.status, 0);
            assert.match(bad.stderr, /checksum mismatch[\s\S]*ACTION:/);
            f.reset();
            writeFileSync(f.response, f.original);
            assert.equal(f.run().status, 0);
        } finally {
            f.cleanup();
        }
    }
});
