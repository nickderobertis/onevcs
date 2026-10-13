"""Release archive layout and sha256 sidecar, retaining the existing asset names."""
import hashlib
import pathlib
import os
import sys
import tarfile
import tomllib
import zipfile

sys.dont_write_bytecode = True
import release_inputs as inputs


def main():
    if len(sys.argv) != 4:
        inputs.fail("expected TARGET BINARY ARCHIVE_DIRECTORY")
    target = inputs.target(sys.argv[1])
    binary = inputs.binary(sys.argv[2], target)
    out = inputs.directory(sys.argv[3])
    version = inputs.version(tomllib.loads(pathlib.Path("crates/onevcs/Cargo.toml").read_text())["package"]["version"])
    files = [binary, *(pathlib.Path(p) for p in ("README.md", "LICENSE", "CHANGELOG.md"))]
    # Preserve upload-rust-binary-action's $tag naming on a release, including a
    # manually cut release; local journeys use the manifest version as their tag.
    tag = os.environ.get("GITHUB_REF_NAME", f"v{version}") if os.environ.get("GITHUB_EVENT_NAME") == "release" else f"v{version}"
    if not tag.startswith("v"):
        inputs.fail(f"release tag {tag!r} is not a version tag")
    inputs.version(tag[1:])
    archive = out / f"onevcs-{tag}-{target}"
    if "windows" in target:
        archive = archive.with_name(archive.name + ".zip")
        with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as packed:
            for path in files:
                packed.write(path, path.name)
    else:
        archive = archive.with_name(archive.name + ".tar.gz")
        with tarfile.open(archive, "w:gz") as packed:
            for path in files:
                packed.add(path, arcname=path.name)
    archive.with_name(archive.name + ".sha256").write_text(
        f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n"
    )


try:
    main()
except (ValueError, OSError, KeyError) as error:
    sys.exit(inputs.diagnostic(error))
