#!/usr/bin/env bash
# Builds `nexa` for Apple Silicon and Intel Macs and fuses them into one
# universal binary with `lipo`. Run on a Mac:
#
#   rustup target add aarch64-apple-darwin x86_64-apple-darwin
#   scripts/build-macos.sh
#
# Output: dist/nexa-{arm64,x86_64,universal}-macos.tar.gz (+ .sha256)
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "build-macos.sh must run on macOS (it needs lipo and the Apple SDK)" >&2
  exit 1
fi

export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-11.0}"
for target in aarch64-apple-darwin x86_64-apple-darwin; do
  cargo build --release --locked -p nexa-cli --target "$target"
done

mkdir -p dist/universal
lipo -create -output dist/universal/nexa \
  target/aarch64-apple-darwin/release/nexa \
  target/x86_64-apple-darwin/release/nexa
lipo -info dist/universal/nexa

# Ad-hoc sign so Gatekeeper on Apple Silicon accepts the fused binary.
codesign --force --sign - dist/universal/nexa

package() {
  local name="$1" dir="$2"
  tar -C "$dir" -czf "dist/nexa-$name-macos.tar.gz" nexa
  (cd dist && shasum -a 256 "nexa-$name-macos.tar.gz" > "nexa-$name-macos.tar.gz.sha256")
}
package arm64 target/aarch64-apple-darwin/release
package x86_64 target/x86_64-apple-darwin/release
package universal dist/universal
ls -lh dist/*.tar.gz
