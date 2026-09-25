#!/usr/bin/env bash
# Herdr plugin build hook (cwd = plugin root). Pattern from herdr-nvim and annotate:
#   1. matching binary already installed                  -> done
#   2. AGENT_PRS_FROM_SOURCE=1, or no release for this OS  -> cargo build
#   3. download release asset, verify SHA256SUMS, probe it -> install
# Any failure in 3 falls back to a source build when cargo is available.
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p bin
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

repo="sebassdc/herdr-agent-prs"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' herdr-plugin.toml | head -1)

install_bin() {
  # Replace, never overwrite in place: macOS kills binaries whose pages change.
  rm -f bin/herdr-agent-prs
  cp "$1" bin/herdr-agent-prs
  chmod +x bin/herdr-agent-prs
  echo "$version" > bin/herdr-agent-prs.version
}

build_from_source() {
  if ! command -v cargo >/dev/null; then
    echo "herdr-agent-prs: no usable prebuilt binary and no cargo to build from source" >&2
    exit 1
  fi
  echo "herdr-agent-prs: building from source" >&2
  cargo build --release --locked
  install_bin target/release/herdr-agent-prs
  echo "herdr-agent-prs: built $version from source"
}

if [ "${AGENT_PRS_FROM_SOURCE:-}" = 1 ]; then
  build_from_source
  exit 0
fi

if [ -x bin/herdr-agent-prs ] && [ "$(cat bin/herdr-agent-prs.version 2>/dev/null)" = "$version" ]; then
  echo "herdr-agent-prs $version already installed"
  exit 0
fi

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64)                target=aarch64-apple-darwin ;;
  Darwin-x86_64)               target=x86_64-apple-darwin ;;
  Linux-x86_64)                target=x86_64-unknown-linux-gnu ;;
  Linux-aarch64|Linux-arm64)   target=aarch64-unknown-linux-gnu ;;
  *) target="" ;;
esac

try_prebuilt() {
  [ -n "$target" ] || return 1
  command -v curl >/dev/null || return 1
  local base="https://github.com/$repo/releases/download/v$version"
  local asset="herdr-agent-prs-$target"
  curl -fsSL --retry 2 -o "$tmp/$asset" "$base/$asset" || return 1
  curl -fsSL --retry 2 -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" || return 1
  local expected actual
  expected="$(grep " $asset\$" "$tmp/SHA256SUMS" | awk '{print $1}')"
  [ -n "$expected" ] || { echo "herdr-agent-prs: $asset missing from SHA256SUMS" >&2; return 1; }
  if command -v sha256sum >/dev/null; then
    actual="$(sha256sum "$tmp/$asset" | awk '{print $1}')"
  else
    actual="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"
  fi
  [ "$actual" = "$expected" ] || { echo "herdr-agent-prs: sha256 mismatch for $asset" >&2; return 1; }
  chmod +x "$tmp/$asset"
  # Probe: no-arg run prints usage and exits 2. Anything else (old glibc,
  # wrong arch, truncated file) means the binary cannot run here.
  local status=0
  "$tmp/$asset" >/dev/null 2>&1 || status=$?
  [ "$status" -eq 2 ] || { echo "herdr-agent-prs: prebuilt $target did not run (exit $status)" >&2; return 1; }
  install_bin "$tmp/$asset"
  echo "herdr-agent-prs: installed $version ($target)"
}

try_prebuilt || build_from_source
