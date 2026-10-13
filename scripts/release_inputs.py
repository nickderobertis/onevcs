"""Private release-boundary validation from the existing npm target authority."""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent


def fail(message):
    raise ValueError(message)


def target(value):
    assembler = (ROOT / "scripts/npm-build.mjs").read_text()
    targets = re.findall(r'^  "([a-z0-9_]+-[a-z0-9_-]+)": \{ platform:', assembler, re.M)
    if not targets:
        fail("cannot read target authority in scripts/npm-build.mjs")
    if value not in targets:
        fail(f"unsupported release target {value!r}")
    return value


def version(value):
    if not isinstance(value, str) or not re.fullmatch(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?", value):
        fail(f"invalid release version {value!r}")
    return value


def binary(value, triple):
    path = pathlib.Path(value)
    expected = "onevcs.exe" if "windows" in triple else "onevcs"
    if not path.is_file() or path.name != expected:
        fail(f"missing release executable {path}; expected {expected}")
    return path


def directory(value):
    path = pathlib.Path(value)
    if not path.is_dir():
        fail(f"missing staging directory {path}")
    return path


def diagnostic(error):
    print(f"release packaging: {error}\nACTION: rebuild the workflow-declared target, then restage its artifacts", file=sys.stderr)
    return 1


if __name__ == "__main__":
    try:
        if len(sys.argv) != 2:
            fail("expected exactly one release target")
        target(sys.argv[1])
    except (ValueError, OSError) as error:
        sys.exit(diagnostic(error))
