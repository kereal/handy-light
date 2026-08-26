# Kereal Optimizations

A running log of every optimization done on top of upstream cjpais/Handy.
Each entry follows: what changed, why, and how to verify or undo.

---

## 1. Drop dead dependencies (`rdev`, `tungstenite`)

**Files:** `src-tauri/Cargo.toml`

**Date:** kereal cycle 1 (2026-08-26)

**Problem:** `cargo machete` flagged two crates that the Rust code no longer
references:

- `rdev` — keyboard hook lib. Last touched in the very first
  commit `83d8452 basic working transcription`. Replaced by `handy-keys`
  + `enigo` long ago; the `use rdev` import was already gone.
- `tungstenite = "0.24"` — direct dep. The WS proxy path uses
  `tokio_tungstenite`, which re-exports the `tungstenite` crate, so the
  direct dep is fully redundant.

**Change:** remove both `rdev` and `tungstenite` lines from
`src-tauri/Cargo.toml`. `Cargo.lock` regenerates on next build.

**Impact:** ~3-5 fewer crates in the dep graph, marginally faster link
time. No effect on runtime size (LTO already drops unused symbols).

**Verify:** `cargo machete` from `src-tauri/` reports no findings.

**Undo:** `git revert <commit>`. Or restore the two lines in
`src-tauri/Cargo.toml` and re-run `cargo check`.

---

## 2. Filter ggml-cpu-*.dll to a portable ISA set

**Files:** `scripts/build-windows-portable.sh` (new)

**Date:** kereal cycle 1 (2026-08-26)

**Problem:** Upstream Handy's Windows release ships every CPU backend
that `ggml-cpu` can build: `sandybridge`, `sse42`, `haswell`, `skylakex`,
`alderlake`, `cannonlake`, `cascadelake`, `icelake`, plus the portable
`x64` baseline. That's 9 DLLs totalling ~9.5 MB. At startup, libtranscribe's
`init_backends` `dlopen`s every one of them and picks the best match for
the current CPU — wasted disk, wasted startup time, 9-way race on
a single machine.

A modern Intel/AMD box (2013+, Haswell / Zen) only ever picks one of two:
`haswell` (AVX2/BMI2) or the `x64` fallback. Everything else (`skylakex`,
`cascadelake`, `alderlake`, `icelake`, `cannonlake`, `sandybridge`,
`sse42`) is dead weight.

**Change:** new script `scripts/build-windows-portable.sh`. It runs
after `cargo xwin build --release`, then copies only `handy.exe`,
`handy_app_lib.dll`, `transcribe.dll`, `ggml.dll`, `ggml-base.dll`,
`ggml-cpu-x64.dll`, `ggml-cpu-haswell.dll` into `/tmp/handy-kereal-0.9.6/`
and tars that.

**Impact:** -4 MB on the tarball, -7 DLLs, faster startup.

**Caveats:**

- The build itself still produces all 9 DLLs (faster, single line change
  in `vendor/transcribe-cpp-sys/bindings/rust/sys/build.rs` could
  truncate the build, but it's an invasive patch for a 4 MB saving).
  The `target/` directory is unaffected — only the packager ignores the
  extras.
- If you run Handy on a pre-Haswell box (Sandy/Ivy Bridge, AMD
  Bulldozer/Piledriver) keep `sandybridge` in the script's
  `KEEP_CPU_DLLS` array. The current pair (`x64` + `haswell`) targets
  Haswell+ and matches the script's `KEEP_CPU_DLLS` list.

**Verify:** `tar tzf /tmp/handy-kereal-0.9.6.tar.gz | grep ggml-cpu`
shows only the two chosen.

**Undo:** delete the script; revert to `cp src-tauri/target/.../release/*.dll`.

---

## 3. Re-vendor `transcribe-cpp-sys` at 0.2.2 (no `+kereal.1` magic)

**Files:** `vendor/transcribe-cpp-sys/`, `src-tauri/Cargo.toml`

**Date:** kereal cycle 1 (2026-08-26)

**Problem:** First vendoring pass was a stale 0.2.0 tree with only the
`Cargo.toml` version bumped to 0.2.2. `transcribe.h` still declared
`TRANSCRIBE_VERSION_PATCH 0`, so the produced `transcribe.dll` exported
version string `0.2.0`. The Rust bindings and tauri-specta export
advertised 0.2.2 → at first model load:

```
native library version mismatch: loaded transcribe library is 0.2.0,
but these bindings were generated for 0.2.2
```

**Change:** re-vendor from the real `v0.2.2` tag at
github.com/handy-computer/transcribe.cpp. Patches preserved (see below).

**Impact:** `transcribe.dll` now actually exports 0.2.2 and the bindings
match.

**Verify:** `strings /tmp/handy-kereal-0.9.6/transcribe.dll | grep '^0.2'`
shows `0.2.2`.

**Undo:** drop `[patch.crates-io] transcribe-cpp-sys` from
`src-tauri/Cargo.toml` and `rm -rf vendor/`. Falls back to crates.io
0.2.2 — but that one breaks on WSL/cargo-xwin (see #4).

---

## 4. Two clang-cl/MSVC patches in `vendor/transcribe-cpp-sys`

**Files:** `vendor/transcribe-cpp-sys/ggml/src/ggml-cpu/CMakeLists.txt`,
`vendor/transcribe-cpp-sys/ggml/src/CMakeLists.txt`

**Date:** kereal cycle 1 (2026-08-26)

**Problem:** The vendored 0.2.2 tree (and any 0.2.x crates.io release)
fails to cross-build from WSL with cargo-xwin + clang-cl:

1. `ggml-cpu-alderlake` (and any backend that enables
   `GGML_AVX_VNNI=ON` under MSVC) fails to compile `sgemm.cpp` with
   `error: always_inline function '_mm256_dpbusd_avx_epi32' requires
   target feature 'avxvnni', but would be inlined into function
   compiled without support for 'avxvnni'`. Upstream defines
   `__AVXVNNI__` but does NOT pass `-mavxvnni` to clang-cl.
2. Every `ggml-cpu-*.dll` backend fails to link with undefined
   `RegOpenKeyExA` / `RegQueryValueExA` / `RegCloseKey`. `ggml-cpu.cpp`
   pokes the Windows Registry for CPU brand detection; upstream 0.2.x
   dropped the Advapi32 link. `MODULE` libraries don't transitively
   propagate `PRIVATE` deps from `ggml-base`, so linking advapi32 on
   `ggml-base` alone is not enough.

**Change:** two local edits in the vendored Cmake files (each marked
`# kereal mod:` in the source):

- `ggml/src/ggml-cpu/CMakeLists.txt`, MSVC+Clang+AVX_VNNI branch:
  append `if (CMAKE_C_COMPILER_ID STREQUAL "Clang") list(APPEND
  ARCH_FLAGS -mavxvnni) endif()`.
- `ggml/src/CMakeLists.txt`, inside `ggml_add_backend_library()`:
  after `target_link_libraries(${backend} PRIVATE ggml-base)` add
  `if (WIN32) target_link_libraries(${backend} PRIVATE advapi32)
  endif()`. Also link `advapi32` to `ggml-base` for the static path
  (macOS / non-DL builds).

**Impact:** Windows cross-build from WSL succeeds. Performance
unchanged (the flag is required for correctness, not speed).

**Verify:** `cargo xwin build --release --target
x86_64-pc-windows-msvc` finishes without `sgemm.cpp` or `RegOpenKeyExA`
errors.

**Undo:** when upstream transcribe-cpp-sys 0.2.3+ ships with both fixes,
drop the vendor and the `[patch.crates-io]` line. Search upstream commits
for `mavxvnni` and `advapi32` in the relevant `CMakeLists.txt` files to
confirm.

---

## 5. Build-portable script + deterministic `0.9.6+kereal.1` version

**Files:** `scripts/build-windows-portable.sh`, `package.json`,
`src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.lock`

**Date:** kereal cycle 1 (2026-08-26)

**Problem (a):** the manual packager flow was a 6-line `for` loop that
had to be re-typed every release. Repeated a wrong wildcard once and
shipped the test `libfoo.dll` next to `handy.exe`.

**Problem (b):** `tauri-plugin-updater` reports our fork as out of
date. The plugin compares the manifest's version (upstream `0.9.6`)
against `package.json`'s version via `semver::Version::cmp`:
`release.version > self.current_version`. With our version string
`0.9.6-kereal.1` (a SemVer pre-release), pre-release precedence
kicks in and `0.9.6 > 0.9.6-kereal.1` is `true` — the updater
always advertises upstream as newer.

**Change (a):** `scripts/build-windows-portable.sh` codifies the packager
flow (see #2 for the DLL filter).

**Change (b):** switch the suffix from `-` to `+` in all three manifests
plus `Cargo.lock`. SemVer build metadata (`+kereal.1`) is ignored for
precedence: `0.9.6+kereal.1` and `0.9.6` compare equal. Visual version
in the title bar, About page, and GitHub release tag is unchanged for
human readers — both still read as `0.9.6 (kereal.1)`.

**Impact:** footer no longer lies about updates. The packager is
re-runnable.

**Verify:** with the built `handy.exe` installed, the footer reads
"Up to date" (not "Update available") when running against the upstream
`latest.json`.

**Undo (a):** delete the script. **Undo (b):** `sed -i 's/+kereal.1/-kereal.1/g'`
in the four files. Re-tag with a non-pre-release comparison only if the
fork actually wants upstream to advertise a newer release.

---

## Audit findings (informational, not yet acted on)

These came out of `cargo machete` and a manual `Cargo.toml` scan but were
left alone — each is intentional, not dead code:

- **`rdev`**: removed (#1).
- **`tungstenite`**: removed (#1).
- **`signal-hook`**: only on `cfg(unix)`, used in `signal_handle.rs`
  for SIGUSR1/2 IPC. Needed for Linux/macOS.
- **`ferrous-opencc`**: used in `actions.rs` for zh-CN → zh-TW
  conversion.
- **`natural` + `strsim`**: used in `audio_toolkit/text.rs` for
  soundex + Levenshtein in the post-process spell-correction path.
- **`reqwest` with json/stream/gzip/brotli/deflate**: used by
  hf-hub to download model files with resumable HTTP. All four
  decoders are needed for different upstream mirror headers.
- **Cuda/DirectML/ROCm strings in `handy.exe`**: feature-gate switch
  code in `transcribe-cpp` — strings live in `.rdata` but contribute
  a few KB; not worth touching.
- **`Cargo.lock` size**: ~1.7 MB. Normal for a project this size.

## Open / not yet done

- **Vulkan support on Windows x86_64**: 4-5× speedup on a discrete GPU.
  `transcribe-cpp` feature `vulkan` is set for Linux and macOS (Metal)
  but not Windows because `vulkan-shaders-gen` needs >6 GB RAM at
  build time and WSL small-machine limits collide. Adding
  `features = ["dynamic-backends", "vulkan"]` to
  `src-tauri/Cargo.toml` `[target.'cfg(all(windows, target_arch =
  "x86_64"))'.dependencies]` enables it — measure RAM before doing so.
- **ICUl/Unicode data size**: `.rdata` is 13 MB, dominated by
  ICU/Unicode tables pulled in by Tauri. Hard to shrink without forking
  Tauri itself.
- **LTO/codegen-units/strip already at maximum aggression**:
  `[profile.release] lto = true, codegen-units = 1, strip = true`.
  Cannot go further without dropping safety (`panic = "abort"` would
  save ~0.5 MB but breaks unwind-required FFI).

## Replay the audit

```bash
cargo install cargo-machete cargo-bloat
cd src-tauri
cargo machete                            # unused deps
cargo bloat --release --crates -n 20     # binary composition (host only)
```
