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
    wheel, = (out / "wheels").glob("*.whl")
    arch = target.split("-")[0]
    with zipfile.ZipFile(wheel) as z:
        payloads = [name for name in z.namelist() if name.endswith(".data/scripts/onevcs") or name.endswith(".data/scripts/onevcs.exe")]
        check(len(payloads) == 1, f"wheel has no unique executable: {payloads}")
        check(z.read(payloads[0]) == source, "wheel binary differs from retained compiled artifact")
        metadata = z.read(next(n for n in z.namelist() if n.endswith(".dist-info/METADATA"))).decode()
        check(f"Version: {version}\n" in metadata, "staged artifact contract mismatch")
        if "apple" in target:
            check(f"_{'arm64' if arch == 'aarch64' else arch}.whl" in wheel.name and "macosx_" in wheel.name, f"wrong Darwin tag: {wheel}")
    carrier, = (out / "npm").glob("onevcs-cli-*-*.tgz")
    with tarfile.open(carrier) as t:
        exe = "onevcs.exe" if "windows" in target else "onevcs"
        check(t.extractfile(f"package/bin/{exe}").read() == source, "npm binary differs")
        manifest = json.load(t.extractfile("package/package.json"))
        check(manifest["version"] == version, "staged artifact contract mismatch")
        check(manifest["os"] == ["darwin" if "apple" in target else "win32" if "windows" in target else "linux"], "npm OS differs from its target")
        check(manifest["cpu"] == ["arm64" if arch == "aarch64" else "x64"], "staged artifact contract mismatch")
    archive, = [p for p in (out / "archives").iterdir() if not p.name.endswith(".sha256")]
    if archive.name.endswith(".zip"):
        with zipfile.ZipFile(archive) as z:
            check(z.read("onevcs.exe") == source, "staged artifact contract mismatch")
    else:
        with tarfile.open(archive) as t:
            check(t.extractfile("onevcs").read() == source, "staged artifact contract mismatch")
            check({"README.md", "LICENSE", "CHANGELOG.md"}.issubset(t.getnames()))
    check(archive.with_name(archive.name + ".sha256").read_text().split()[0] == hashlib.sha256(archive.read_bytes()).hexdigest(), "staged artifact contract mismatch")
    (out / "payload-hashes.json").write_text(json.dumps({"target": target, "version": version, "archive": digest, "wheel": digest, "npm": digest}) + "\n")


try:
    main()
except (ValueError, OSError, KeyError, tarfile.TarError, zipfile.BadZipFile, struct.error) as error:
    sys.exit(inputs.diagnostic(error))
