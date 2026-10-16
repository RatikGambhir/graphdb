#!/usr/bin/env bash
# Builds `ratik` for Apple Silicon and Intel Macs and fuses them into one
# universal binary with `lipo`. Run on a Mac:
#
#   rustup target add aarch64-apple-darwin x86_64-apple-darwin
#   scripts/build-macos.sh
#
# Output: dist/ratik-{arm64,x86_64,universal}-macos.tar.gz (+ .sha256)
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "build-macos.sh must run on macOS (it needs lipo and the Apple SDK)" >&2
  exit 1
fi

export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-11.0}"
for target in aarch64-apple-darwin x86_64-apple-darwin; do
  cargo build --release --locked -p ratik-cli --target "$target"
done

mkdir -p dist/universal
lipo -create -output dist/universal/ratik \
  target/aarch64-apple-darwin/release/ratik \
  target/x86_64-apple-darwin/release/ratik
lipo -info dist/universal/ratik

# Ad-hoc sign so Gatekeeper on Apple Silicon accepts the fused binary.
codesign --force --sign - dist/universal/ratik

package() {
  local name="$1" dir="$2"
  tar -C "$dir" -czf "dist/ratik-$name-macos.tar.gz" ratik
  (cd dist && shasum -a 256 "ratik-$name-macos.tar.gz" > "ratik-$name-macos.tar.gz.sha256")
}
package arm64 target/aarch64-apple-darwin/release
package x86_64 target/x86_64-apple-darwin/release
package universal dist/universal
ls -lh dist/*.tar.gz
