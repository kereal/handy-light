#!/usr/bin/env bash
# Build a portable Windows tarball from the cargo xwin release output.
#
# kereal mod: filters ggml-cpu-*.dll to the ISA variants we actually need
# (x64 baseline + haswell). Upstream Handy ships all 9 ISAs (sandybridge,
# sse42, haswell, skylakex, alderlake, cannonlake, cascadelake, icelake,
# x64) totalling ~9.5 MB; for a single-machine install that's wasted bytes
# plus a 9-way dlopen race at startup.
#
# Usage:  scripts/build-windows-portable.sh
# Output: /tmp/handy-kereal-0.9.6.tar.gz
set -euo pipefail

cd "$(dirname "$0")/.."

# Sanity: have we built the Windows release?
if [ ! -f src-tauri/target/x86_64-pc-windows-msvc/release/handy.exe ]; then
  echo "missing handy.exe — run: cargo xwin build --release \\"
  echo "  --manifest-path src-tauri/Cargo.toml \\"
  echo "  --target x86_64-pc-windows-msvc \\"
  echo "  --features tauri/custom-protocol"
  exit 1
fi

# Filter: keep only x64 (baseline) + haswell (Haswell+ AVX2/BMI2).
# Add/remove ISA names here as needed; see vendor/transcribe-cpp-sys/ggml/src
# for the full list of available variants.
KEEP_CPU_DLLS=(
  "ggml-cpu-x64.dll"
  "ggml-cpu-haswell.dll"
)

DEST=/tmp/handy-kereal-0.9.6
rm -rf "$DEST"
mkdir -p "$DEST"

# Required: handy.exe + handy_app_lib.dll + transcribe.dll + ggml base libs
for f in handy.exe handy_app_lib.dll transcribe.dll ggml.dll ggml-base.dll; do
  cp "src-tauri/target/x86_64-pc-windows-msvc/release/$f" "$DEST/"
done

# Filtered: CPU backends
for f in "${KEEP_CPU_DLLS[@]}"; do
  cp "src-tauri/target/x86_64-pc-windows-msvc/release/$f" "$DEST/"
done

cd "$DEST"
tar czf /tmp/handy-kereal-0.9.6.tar.gz .

echo
echo "Built: /tmp/handy-kereal-0.9.6.tar.gz"
ls -lh /tmp/handy-kereal-0.9.6.tar.gz
echo
echo "DLLs in archive:"
tar tzf /tmp/handy-kereal-0.9.6.tar.gz | sort
