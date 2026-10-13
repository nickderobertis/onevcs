"""Validate staged release payloads, their wheel tags and byte-for-byte reuse."""
import hashlib
import json
import pathlib
import sys
import struct
import tarfile
import zipfile

sys.dont_write_bytecode = True
import release_inputs as inputs


def check(condition, message="staged artifact contents do not match the release contract"):
    if not condition:
        inputs.fail(message)


def tar_member(packed, name):
    try:
        stream = packed.extractfile(name)
    except KeyError:
        inputs.fail(f"archive is missing required member {name}")
    if stream is None:
        inputs.fail(f"archive member {name} is not a file")
    return stream


# llmlint: ignore[changed_behavior_has_e2e] onevcs-scripts-e2e:release-packaging drives this path with real artifacts and corruption rejection on Linux in release.yml build-npm before publication; it stays outside blanket check to avoid a release build on every change.
def main():
    if len(sys.argv) != 5:
        inputs.fail("expected TARGET BINARY STAGING_DIRECTORY VERSION")
    target = inputs.target(sys.argv[1])
    binary = inputs.binary(sys.argv[2], target)
    output = inputs.directory(sys.argv[3])
    version = inputs.version(sys.argv[4])
    out = pathlib.Path(output)
    source = pathlib.Path(binary).read_bytes()
    arch = target.split("-")[0]
    # payload.test.mjs drives these foreign headers through the real artifact
    # boundary without executing them on Linux. release-darwin.sh runs real
    # Darwin/Rosetta products; the retained Windows release jobs own PE execution.
    if "apple" in target:
        check(source[:4] == b"\xcf\xfa\xed\xfe", "Darwin payload is not a 64-bit Mach-O executable")
        check(struct.unpack_from("<I", source, 4)[0] == (0x0100000C if arch == "aarch64" else 0x01000007), "Darwin executable architecture differs from its target")
    elif "linux" in target:
        check(source[:6] == b"\x7fELF\x02\x01", "Linux payload is not a little-endian ELF64 executable")
        check(struct.unpack_from("<H", source, 18)[0] == (183 if arch == "aarch64" else 62), "Linux executable architecture differs from its target")
    else:
        check(source[:2] == b"MZ", "Windows payload is not a PE executable")
        offset = struct.unpack_from("<I", source, 60)[0]
        check(source[offset:offset+4] == b"PE\0\0" and struct.unpack_from("<H", source, offset+4)[0] == 0x8664, "Windows executable is not x64")
    digest = hashlib.sha256(source).hexdigest()
    wheel = inputs.one((out / "wheels").glob("*.whl"), "staged wheel")
    arch = target.split("-")[0]
    with zipfile.ZipFile(wheel) as z:
        payloads = [name for name in z.namelist() if name.endswith(".data/scripts/onevcs") or name.endswith(".data/scripts/onevcs.exe")]
        check(len(payloads) == 1, f"wheel has no unique executable: {payloads}")
        check(z.read(payloads[0]) == source, "wheel binary differs from retained compiled artifact")
        metadata_entries = [n for n in z.namelist() if n.endswith(".dist-info/METADATA")]
        check(len(metadata_entries) == 1, "wheel needs exactly one METADATA member")
        metadata = z.read(metadata_entries[0]).decode()
        check(f"Version: {version}\n" in metadata, "staged artifact contract mismatch")
        if "apple" in target:
            check(f"_{'arm64' if arch == 'aarch64' else arch}.whl" in wheel.name and "macosx_" in wheel.name, f"wrong Darwin tag: {wheel}")
    packages = []
    for carrier in (out / "npm").glob("*.tgz"):
        with tarfile.open(carrier) as t:
            manifest = json.load(tar_member(t, "package/package.json"))
            check(isinstance(manifest, dict) and {"name", "version"}.issubset(manifest), "npm manifest needs name and version fields")
            packages.append(manifest)
            if manifest["name"] == "onevcs-cli":
                check(isinstance(manifest.get("bin"), dict) and manifest["bin"].get("onevcs") == "bin/onevcs.js", "npm launcher needs its onevcs entry point")
                check(tar_member(t, "package/bin/onevcs.js").read() == (inputs.ROOT / "npm/onevcs/bin/onevcs.js").read_bytes(), "npm launcher code differs from this revision")
            else:
                exe = "onevcs.exe" if "windows" in target else "onevcs"
                check(tar_member(t, f"package/bin/{exe}").read() == source, "npm binary differs")
                check(manifest["version"] == version, "npm carrier version differs")
                check(isinstance(manifest.get("os"), list) and all(isinstance(value, str) for value in manifest["os"]), "npm carrier os must be a string array")
                check(isinstance(manifest.get("cpu"), list) and all(isinstance(value, str) for value in manifest["cpu"]), "npm carrier cpu must be a string array")
                check(manifest["os"] == ["darwin" if "apple" in target else "win32" if "windows" in target else "linux"], "npm OS differs from its target")
                check(manifest["cpu"] == ["arm64" if arch == "aarch64" else "x64"], "npm CPU differs from its target")
    launchers = [p for p in packages if p["name"] == "onevcs-cli"]
    carriers = [p for p in packages if p["name"] != "onevcs-cli"]
    check(len(launchers) == len(carriers) == 1, "staging needs exactly one npm carrier and launcher")
    launcher = launchers[0]
    check(launcher["version"] == version, "npm launcher version differs")
    check(isinstance(launcher.get("optionalDependencies"), dict), "npm launcher needs an optionalDependencies object")
    check(launcher["optionalDependencies"].get(carriers[0]["name"]) == version, "npm launcher does not pin its staged carrier")
    check(all(v == version for v in launcher["optionalDependencies"].values()), "npm launcher platform pins differ from its version")
    archive = inputs.one([p for p in (out / "archives").iterdir() if not p.name.endswith(".sha256")], "staged archive")
    if archive.name.endswith(".zip"):
        with zipfile.ZipFile(archive) as z:
            check(z.read("onevcs.exe") == source, "staged artifact contract mismatch")
    else:
        with tarfile.open(archive) as t:
            check(tar_member(t, "onevcs").read() == source, "staged artifact contract mismatch")
            check({"README.md", "LICENSE", "CHANGELOG.md"}.issubset(t.getnames()))
    checksum = archive.with_name(archive.name + ".sha256").read_text().split()
    check(len(checksum) == 2, "archive checksum needs a digest and filename")
    check(checksum[0] == hashlib.sha256(archive.read_bytes()).hexdigest() and checksum[1] == archive.name, "archive checksum does not match its staged archive")
    (out / "payload-hashes.json").write_text(json.dumps({"schema_version": 2, "target": target, "version": version, "archive_payload_sha256": digest, "wheel_payload_sha256": digest, "npm_payload_sha256": digest}) + "\n")


try:
    main()
except (ValueError, OSError, KeyError, tarfile.TarError, zipfile.BadZipFile, struct.error) as error:
    sys.exit(inputs.diagnostic(error))
