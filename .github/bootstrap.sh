#!/usr/bin/env bash
# Private workflow tools follow the committed Node lock and actionlint pin.
set -euo pipefail
npm ci --prefix .github --ignore-scripts --no-audit --no-fund --silent
version=1.7.12
mkdir -p .github/bin
if [ ! -x .github/bin/actionlint ] && [ ! -x .github/bin/actionlint.exe ]; then
  case "$(uname -s)" in
    Linux) os=linux ;;
    Darwin) os=darwin ;;
    MINGW*|MSYS*) os=windows ;;
    *) echo 'workflow tools: unsupported host; ACTION: install actionlint 1.7.12 for this host' >&2; exit 1 ;;
  esac
  case "$(uname -m)" in
    x86_64) arch=amd64 ;;
    arm64|aarch64) arch=arm64 ;;
    *) echo 'workflow tools: unsupported architecture; ACTION: install actionlint 1.7.12 for this host architecture' >&2; exit 1 ;;
  esac
  extension=tar.gz
  [ "$os" != windows ] || extension=zip
  asset="actionlint_${version}_${os}_${arch}.$extension"
  curl --fail --silent --show-error --location "https://github.com/rhysd/actionlint/releases/download/v${version}/$asset" -o ".github/bin/$asset"
  python3 - "$asset" <<'PYTHON'
import hashlib
import pathlib
import sys
asset = sys.argv[1]
expected = dict(line.split()[::-1] for line in pathlib.Path(".github/actionlint-checksums.txt").read_text().splitlines())
actual = hashlib.sha256(pathlib.Path(".github/bin", asset).read_bytes()).hexdigest()
if expected.get(asset) != actual:
    raise SystemExit("actionlint checksum mismatch; remove .github/bin and retry bootstrap")
PYTHON
  if [ "$os" = windows ]; then
    python3 - "$asset" <<'PYTHON'
import pathlib
import sys
import zipfile
with zipfile.ZipFile(pathlib.Path(".github/bin", sys.argv[1])) as packed:
    packed.extract("actionlint.exe", ".github/bin")
PYTHON
  else
    tar -xzf ".github/bin/$asset" -C .github/bin
  fi
  rm ".github/bin/$asset"
fi
