"""Retain maturin's compiled wheel executable as the sole archive/npm input."""
import pathlib
import sys
import zipfile

sys.dont_write_bytecode = True
import release_inputs as inputs


try:
    if len(sys.argv) != 4:
        inputs.fail("expected TARGET WHEEL_DIRECTORY BINARY_DIRECTORY")
    target = inputs.target(sys.argv[1])
    wheels = inputs.directory(sys.argv[2])
    output = inputs.directory(sys.argv[3])
    wheel = inputs.one(wheels.glob("*.whl"), "compiled wheel")
    executable = "onevcs.exe" if "windows" in target else "onevcs"
    with zipfile.ZipFile(wheel) as packed:
        names = [name for name in packed.namelist() if name.endswith(f".data/scripts/{executable}")]
        if len(names) != 1:
            inputs.fail(f"wheel {wheel} has no unique {executable} payload")
        destination = output / executable
        destination.write_bytes(packed.read(names[0]))
        destination.chmod(0o755)
    print(destination)
except (ValueError, OSError, KeyError, zipfile.BadZipFile) as error:
    sys.exit(inputs.diagnostic(error))
