#!/usr/bin/env bash
# Private workflow tools follow the committed Node lock and actionlint pin.
set -euo pipefail
die() { printf 'workflow tools: %s\nACTION: %s\n' "$1" "$2" >&2; exit 1; }
npm ci --prefix .github --ignore-scripts --no-audit --no-fund --silent || die 'npm ci failed' 'check registry/cache access and retry bootstrap'
version=1.7.12
mkdir -p .github/bin
if [ ! -x .github/bin/actionlint ] && [ ! -x .github/bin/actionlint.exe ]; then
  case "$(uname -s)" in
    Linux) os=linux ;;
    Darwin) os=darwin ;;
    MINGW*|MSYS*) os=windows ;;
    *) die 'unsupported host' 'install actionlint 1.7.12 for this host' ;;
  esac
  case "$(uname -m)" in
    x86_64) arch=amd64 ;;
    arm64|aarch64) arch=arm64 ;;
    *) die 'unsupported architecture' 'install actionlint 1.7.12 for this architecture' ;;
  esac
  extension=tar.gz
  [ "$os" != windows ] || extension=zip
  asset="actionlint_${version}_${os}_${arch}.$extension"
  curl --fail --silent --show-error --location "https://github.com/rhysd/actionlint/releases/download/v${version}/$asset" -o ".github/bin/$asset" || die 'actionlint download failed' 'check the pinned release asset URL and retry bootstrap'
  python3 - "$asset" <<'PYTHON' || die 'actionlint integrity check failed' 'restore the pinned checksum table and retry bootstrap'
import hashlib
import pathlib
import re
import sys
asset = sys.argv[1]
expected = {}
for index, line in enumerate(pathlib.Path(".github/actionlint-checksums.txt").read_text().splitlines(), 1):
    fields = line.split()
    if len(fields) != 2 or not re.fullmatch(r"[0-9a-f]{64}", fields[0]) or not re.fullmatch(r"[A-Za-z0-9_.-]+", fields[1]) or fields[1] in expected:
        raise SystemExit(f"invalid checksum row {index}; ACTION: restore the pinned checksum table")
    expected[fields[1]] = fields[0]
actual = hashlib.sha256(pathlib.Path(".github/bin", asset).read_bytes()).hexdigest()
if expected.get(asset) != actual:
    raise SystemExit("actionlint checksum mismatch; ACTION: remove .github/bin and retry bootstrap")
PYTHON
  if [ "$os" = windows ]; then
    python3 - "$asset" <<'PYTHON' || die 'actionlint zip extraction failed' 'remove the cached archive and retry bootstrap'
import pathlib
import sys
import zipfile
with zipfile.ZipFile(pathlib.Path(".github/bin", sys.argv[1])) as packed:
    packed.extract("actionlint.exe", ".github/bin")
PYTHON
  else
    tar -xzf ".github/bin/$asset" -C .github/bin || die 'actionlint tar extraction failed' 'remove the cached archive and retry bootstrap'
  fi
  rm ".github/bin/$asset"
fi
