import test from "node:test";
import assert from "node:assert/strict";
import {
    mkdtempSync,
    mkdirSync,
    writeFileSync,
    readFileSync,
    rmSync,
} from "node:fs";
import { resolve, join } from "node:path";
import { execFileSync, spawnSync } from "node:child_process";
import { workflows } from "../workflow-graph.mjs";
// Foreign platform fixtures prove the artifact/header boundary only. They are
// never executed here; release-darwin.sh supplies real native/Rosetta execution.
test("foreign artifact contracts validate declared Mach-O/PE architectures and reject damaged package forms", () => {
    const base =
        process.env.ONEPIPELINE_NODE_SCRATCH_DIR ||
        resolve(".github/.telemetry");
    mkdirSync(base, { recursive: true });
    const dir = mkdtempSync(join(base, "payload-"));
    const wf = workflows()["release.yml"];
    const targets = [
        ...wf.jobs["build-darwin"].env.DARWIN_TARGETS.split(" "),
        ...wf.jobs["build-npm"].strategy.matrix.include
            .filter((r) => r.target.includes("windows"))
            .map((r) => r.target),
    ];
    try {
        for (const target of targets) {
            const out = join(dir, target);
            for (const sub of ["binary", "wheels", "npm", "archives"])
                mkdirSync(join(out, sub), { recursive: true });
            const windows = target.includes("windows"),
                arm = target.startsWith("aarch64");
            const bytes = Buffer.alloc(128);
            if (windows) {
                bytes.write("MZ");
                bytes.writeUInt32LE(64, 60);
                bytes.write("PE\0\0", 64);
                bytes.writeUInt16LE(0x8664, 68);
            } else {
                bytes.writeUInt32LE(0xfeedfacf, 0);
                bytes.writeUInt32LE(arm ? 0x0100000c : 0x01000007, 4);
            }
            const binary = join(
                out,
                "binary",
                windows ? "onevcs.exe" : "onevcs",
            );
            writeFileSync(binary, bytes);
            const fixture = JSON.stringify({
                target,
                out,
                binary,
                windows,
                arm,
            });
            const build = `import json,sys,pathlib,zipfile,tarfile,io\nd=json.loads(sys.argv[1]);o=pathlib.Path(d['out']);b=pathlib.Path(d['binary']).read_bytes();v='0.0.0';name='onevcs.exe' if d['windows'] else 'onevcs'\ntag='win_amd64' if d['windows'] else 'macosx_11_0_arm64' if d['arm'] else 'macosx_10_12_x86_64'\nwith zipfile.ZipFile(o/'wheels'/f'onevcs_cli-{v}-py3-none-{tag}.whl','w') as z:\n z.writestr(f'onevcs_cli-{v}.data/scripts/{name}',b);z.writestr(f'onevcs_cli-{v}.dist-info/METADATA',f'Name: onevcs-cli\\nVersion: {v}\\n')\nplatform='win32' if d['windows'] else 'darwin';cpu='arm64' if d['arm'] else 'x64';carrier=f'onevcs-cli-{platform}-{cpu}'\nfor pkg,manifest,payload in [(carrier,{'name':carrier,'version':v,'os':[platform],'cpu':[cpu]},b),('onevcs-cli',{'name':'onevcs-cli','version':v,'bin':{'onevcs':'bin/onevcs.js'},'optionalDependencies':{carrier:v}},None)]:\n with tarfile.open(o/'npm'/f'{pkg}-{v}.tgz','w:gz') as t:\n  data=json.dumps(manifest).encode();i=tarfile.TarInfo('package/package.json');i.size=len(data);t.addfile(i,io.BytesIO(data))\n  if payload is None:\n   data=pathlib.Path('npm/onevcs/bin/onevcs.js').read_bytes();i=tarfile.TarInfo('package/bin/onevcs.js');i.size=len(data);t.addfile(i,io.BytesIO(data))\n  if payload is not None:\n   i=tarfile.TarInfo(f'package/bin/{name}');i.size=len(payload);t.addfile(i,io.BytesIO(payload))\n`;
            execFileSync("python3", ["-c", build, fixture]);
            execFileSync("python3", [
                "scripts/release-archive.py",
                target,
                binary,
                join(out, "archives"),
            ]);
            const run = () =>
                spawnSync(
                    "python3",
                    [
                        "scripts/release-payload-check.py",
                        target,
                        binary,
                        out,
                        "0.0.0",
                    ],
                    { encoding: "utf8" },
                );
            assert.equal(run().status, 0);
            if (target === "x86_64-apple-darwin")
                assert.deepEqual(
                    JSON.parse(readFileSync(join(out, "payload-hashes.json"))),
                    JSON.parse(
                        readFileSync(
                            ".github/tests/fixtures/payload-hashes.golden.json",
                        ),
                    ),
                );
            const wheel = execFileSync(
                "python3",
                [
                    "-c",
                    `import pathlib,sys;print(next(pathlib.Path(sys.argv[1]).glob('*.whl')))`,
                    join(out, "wheels"),
                ],
                { encoding: "utf8" },
            ).trim();
            const saved = readFileSync(wheel);
            execFileSync("python3", [
                "-c",
                `import zipfile,sys;z=zipfile.ZipFile(sys.argv[1],'w');z.writestr('onevcs_cli-0.0.0.data/scripts/${windows ? "onevcs.exe" : "onevcs"}',open(sys.argv[2],'rb').read());z.close()`,
                wheel,
                binary,
            ]);
            const missingMetadata = run();
            assert.notEqual(missingMetadata.status, 0);
            assert.match(missingMetadata.stderr, /METADATA/);
            assert.match(missingMetadata.stderr, /ACTION:/);
            writeFileSync(wheel, saved);
            const sidecar = execFileSync(
                "python3",
                [
                    "-c",
                    `import pathlib,sys;print(next(pathlib.Path(sys.argv[1]).glob('*.sha256')))`,
                    join(out, "archives"),
                ],
                { encoding: "utf8" },
            ).trim();
            const checksum = readFileSync(sidecar);
            writeFileSync(sidecar, "wrong checksum\n");
            assert.notEqual(run().status, 0);
            writeFileSync(sidecar, checksum);
            const carrier = execFileSync(
                "python3",
                [
                    "-c",
                    `import pathlib,sys;print(next(p for p in pathlib.Path(sys.argv[1]).glob('*.tgz') if 'cli-0.0.0' not in p.name))`,
                    join(out, "npm"),
                ],
                { encoding: "utf8" },
            ).trim();
            const npm = readFileSync(carrier);
            execFileSync("python3", [
                "-c",
                `import tarfile,sys;tarfile.open(sys.argv[1],'w:gz').close()`,
                carrier,
            ]);
            const missingManifest = run();
            assert.notEqual(missingManifest.status, 0);
            assert.match(missingManifest.stderr, /package\/package.json/);
            assert.match(missingManifest.stderr, /ACTION:/);
            writeFileSync(carrier, npm);
            for (const field of ["name", "version", "os", "cpu"]) {
                execFileSync("python3", [
                    "-c",
                    "import tarfile,json,sys,io; p=sys.argv[1]; field=sys.argv[2]; t=tarfile.open(p); files=[(m,t.extractfile(m).read()) for m in t.getmembers() if m.isfile()]; t.close(); t=tarfile.open(p,'w:gz');\nfor m,data in files:\n if m.name=='package/package.json':\n  value=json.loads(data);value[field]=[123] if field in ('name','version') else 'bad-array';data=json.dumps(value).encode()\n m.size=len(data);t.addfile(m,io.BytesIO(data))\nt.close()",
                    carrier,
                    field,
                ]);
                const invalid = run();
                assert.notEqual(invalid.status, 0);
                assert.match(
                    invalid.stderr,
                    field === "name" || field === "version"
                        ? /name\/version must be non-empty strings/
                        : /must be a string array/,
                );
                assert.match(invalid.stderr, /ACTION:/);
                writeFileSync(carrier, npm);
            }
            const wrong = Buffer.from(bytes);
            wrong.writeUInt32LE(0, windows ? 68 : 4);
            writeFileSync(binary, wrong);
            const architecture = run();
            assert.notEqual(architecture.status, 0);
            assert.match(architecture.stderr, /architecture|not x64/);
            writeFileSync(binary, bytes);
            assert.equal(
                run().status,
                0,
                "recovery after all fixture corruptions",
            );
        }
    } finally {
        rmSync(dir, { recursive: true, force: true });
    }
});
