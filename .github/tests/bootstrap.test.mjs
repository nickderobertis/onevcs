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
test("real bootstrap selects and verifies Windows archive through a download transport double", () => {
    const base =
        process.env.ONEPIPELINE_NODE_SCRATCH_DIR ||
        resolve(".github/.telemetry");
    mkdirSync(base, { recursive: true });
    const scratch = mkdtempSync(join(base, "bootstrap-"));
    try {
        mkdirSync(join(scratch, ".github"));
        mkdirSync(join(scratch, "transport"));
        mkdirSync(join(scratch, "archive"));
        for (const file of [
            "bootstrap.sh",
            "package.json",
            "package-lock.json",
        ])
            copyFileSync(`.github/${file}`, join(scratch, ".github", file));
        writeFileSync(
            join(scratch, "archive/actionlint.exe"),
            "transport fixture: a Windows executable is not run on this host\n",
        );
        execFileSync("python3", [
            "-c",
            "import zipfile,sys;z=zipfile.ZipFile(sys.argv[1],'w');z.write(sys.argv[2],'actionlint.exe');z.close()",
            join(scratch, "windows.zip"),
            join(scratch, "archive/actionlint.exe"),
        ]);
        assert.match(
            readFileSync(".github/actionlint-checksums.txt", "utf8"),
            /actionlint_1\.7\.12_windows_amd64\.zip/,
        );
        const hash = createHash("sha256")
            .update(readFileSync(join(scratch, "windows.zip")))
            .digest("hex");
        // Replace only the download-boundary digest with this forge response's digest.
        writeFileSync(
            join(scratch, ".github/actionlint-checksums.txt"),
            `${hash}  actionlint_1.7.12_windows_amd64.zip\n`,
        );
        writeFileSync(
            join(scratch, "transport/uname"),
            '#!/bin/bash\ncase "$1" in -s) echo MINGW64_NT-10.0;; -m) echo x86_64;; *) exit 1;; esac\n',
            { mode: 0o755 },
        );
        writeFileSync(
            join(scratch, "transport/curl"),
            `#!/bin/bash\nset -eu\nurl=""\noutput=""\nwhile [ "$#" -gt 0 ]; do\n case "$1" in https:*) url="$1";; -o) shift; output="$1";; esac\n shift\ndone\nprintf '%s\\n' "$url" >> "$BOOTSTRAP_REQUESTS"\ncp "$BOOTSTRAP_RESPONSE" "$output"\n`,
            { mode: 0o755 },
        );
        const env = {
            ...process.env,
            PATH: join(scratch, "transport") + delimiter + process.env.PATH,
            npm_config_offline: "true",
            BOOTSTRAP_RESPONSE: join(scratch, "windows.zip"),
            BOOTSTRAP_REQUESTS: join(scratch, "requests"),
        };
        const run = () =>
            spawnSync("bash", [".github/bootstrap.sh"], {
                cwd: scratch,
                env,
                encoding: "utf8",
            });
        const good = run();
        assert.equal(good.status, 0, good.stderr);
        assert.ok(existsSync(join(scratch, ".github/bin/actionlint.exe")));
        assert.match(
            readFileSync(join(scratch, "requests"), "utf8"),
            /v1\.7\.12\/actionlint_1\.7\.12_windows_amd64\.zip/,
        );
        // Retry with a corrupt forge payload: the installer must reject its checksum.
        rmSync(join(scratch, ".github/bin/actionlint.exe"));
        writeFileSync(join(scratch, "windows.zip"), "corrupt response");
        const bad = run();
        assert.notEqual(bad.status, 0);
        assert.match(bad.stderr, /checksum mismatch/);
        assert.match(bad.stderr, /retry bootstrap/);
    } finally {
        rmSync(scratch, { recursive: true, force: true });
    }
});
