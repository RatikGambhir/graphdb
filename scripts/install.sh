#!/usr/bin/env bash
# Installs the latest `nexa` binary.
#
#   curl -fsSL https://raw.githubusercontent.com/RatikGambhir/graphdb/main/scripts/install.sh | bash
#
# Set NEXA_VERSION=v0.1.0 to pin a version, NEXA_INSTALL_DIR to choose
# where the binary goes (default: /usr/local/bin if writable, else ~/.local/bin).
set -euo pipefail

repo="RatikGambhir/graphdb"
version="${NEXA_VERSION:-latest}"

case "$(uname -s)" in
  Darwin) asset="nexa-universal-macos.tar.gz" ;;
  Linux)
    [[ "$(uname -m)" == "x86_64" ]] || { echo "prebuilt Linux binaries are x86_64 only; build from source with cargo" >&2; exit 1; }
    asset="nexa-x86_64-linux.tar.gz" ;;
  *) echo "unsupported OS: $(uname -s)" >&2; exit 1 ;;
esac

if [[ "$version" == "latest" ]]; then
  url="https://github.com/$repo/releases/latest/download/$asset"
else
  url="https://github.com/$repo/releases/download/$version/$asset"
fi

dir="${NEXA_INSTALL_DIR:-}"
if [[ -z "$dir" ]]; then
  if [[ -w /usr/local/bin ]]; then dir=/usr/local/bin; else dir="$HOME/.local/bin"; fi
fi
mkdir -p "$dir"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
echo "▸ downloading $url"
curl -fsSL "$url" -o "$tmp/nexa.tar.gz"
curl -fsSL "$url.sha256" -o "$tmp/nexa.tar.gz.sha256" && (
  cd "$tmp" && expected="$(cut -d' ' -f1 nexa.tar.gz.sha256)" &&
  actual="$(shasum -a 256 nexa.tar.gz | cut -d' ' -f1)" &&
  [[ "$expected" == "$actual" ]] || { echo "checksum mismatch" >&2; exit 1; }
)
tar -xzf "$tmp/nexa.tar.gz" -C "$tmp"
install -m 0755 "$tmp/nexa" "$dir/nexa"
# Downloaded binaries are quarantined by macOS; this one is checksum-verified.
[[ "$(uname -s)" == "Darwin" ]] && xattr -d com.apple.quarantine "$dir/nexa" 2>/dev/null || true

echo "✔ installed $("$dir/nexa" --version) to $dir/nexa"
case ":$PATH:" in *":$dir:"*) ;; *) echo "  add $dir to your PATH" ;; esac
echo "  next: nexa init && nexa start --apply schema.nql && nexa shell"
