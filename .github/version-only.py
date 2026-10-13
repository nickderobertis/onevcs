"""Compare parsed TOML; erase only this repository's own package versions."""
import copy
import json
import re
import sys
import tomllib

names = {"onevcs", "onevcs-testing"}
version = re.compile(r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$")


def erase_version(item, requirement=False):
    value = item.get("version")
    prefix = "=" if requirement and isinstance(value, str) and value.startswith("=") else ""
    if not isinstance(value, str) or not version.fullmatch(value.removeprefix(prefix) if prefix else value):
        raise ValueError("invalid release version")
    item["version"] = prefix + "<release>"


def normalized(text, path):
    doc = copy.deepcopy(tomllib.loads(text))
    if path.endswith("Cargo.lock"):
        local = {(p["name"], p["version"]) for p in doc.get("package", [])
                 if p.get("name") in names and "source" not in p}
        for package in doc.get("package", []):
            if "dependencies" in package:
                package["dependencies"] = [
                    f"{parts[0]} <release>" if len(parts := entry.split()) == 2
                    and tuple(parts) in local else entry
                    for entry in package["dependencies"]
                ]
            if package.get("name") in names and "source" not in package:
                erase_version(package)
    else:
        package = doc.get("package", {})
        if package.get("name") not in names:
            raise ValueError("not a release package")
        erase_version(package)
        for section in ("dependencies", "dev-dependencies", "build-dependencies"):
            for name, dependency in doc.get(section, {}).items():
                if name in names and isinstance(dependency, dict) and "path" in dependency:
                    if "version" in dependency:
                        erase_version(dependency, requirement=True)
    return doc


try:
    if len(sys.argv) != 2:
        raise ValueError("expected one semantic file path")
    data = json.load(sys.stdin)
    before = tomllib.loads(data["before"])
    after = tomllib.loads(data["after"])
    print(str(before != after and normalized(data["before"], sys.argv[1]) == normalized(data["after"], sys.argv[1])).lower())
except (ValueError, KeyError, TypeError):
    print("false")
