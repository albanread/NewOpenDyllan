#!/usr/bin/env bash
# tools/self-rebuild.sh — Sprint 60 goal 1 (bash variant of self-rebuild.ps1).
# Run from anywhere; anchors at the workspace root. Needs MSVC link.exe on PATH
# for the cargo build steps (build.rs links nod-driver). See self-rebuild.ps1 for
# the per-step rationale.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DRIVER="$ROOT/target/debug/nod-driver.exe"
PRJ="$ROOT/compiler/dylan-lex-shim.prj"
SHIM="$ROOT/compiler/dylan-lex-shim.lib.obj"

sha() { sha256sum "$1" | cut -d' ' -f1; }

build_shim() {
  # --parse-with-rust is MANDATORY: the running driver already links a shim, so
  # the default Dylan front-end would collide on the shim's own classes
  # ("class redefinition refused", exit 1). See self-rebuild.ps1.
  "$DRIVER" --parse-with-rust build --library --project "$PRJ" -o "$1"
  [ -f "$1" ] || { echo "shim build produced no $1" >&2; exit 1; }
}

echo "== STEP 1: cargo build -p nod-driver =="
( cd "$ROOT" && cargo build -p nod-driver )

echo "== STEP 2: rebuild shim FROM the driver =="
build_shim "$SHIM"
echo "   shim sha256 $(sha "$SHIM")"

echo "== STEP 3: rebuild driver so build.rs relinks the fresh shim =="
( cd "$ROOT" && cargo build -p nod-driver )

echo "== STEP 4a: reproducibility — build twice, compare =="
A="${TMPDIR:-/tmp}/shim-repro-A.obj"; B="${TMPDIR:-/tmp}/shim-repro-B.obj"
build_shim "$A"; build_shim "$B"
HA="$(sha "$A")"; HB="$(sha "$B")"
echo "   A=$HA"; echo "   B=$HB"
[ "$HA" = "$HB" ] || { echo "NON-REPRODUCIBLE: $HA vs $HB" >&2; exit 1; }
[ "$(sha "$SHIM")" = "$HA" ] || { echo "on-disk shim != fresh build" >&2; exit 1; }
echo "   OK: shim .obj byte-identical across rebuilds."

echo "== STEP 4b: front-end output stable across the rebuild =="
for f in hello factorial point mutual sprint09-add; do
  p="$ROOT/tests/nod-tests/fixtures/$f.dylan"; [ -f "$p" ] || continue
  for cmd in dump-dylan-ast dump-tokens dump-dfm; do
    h1="$("$DRIVER" "$cmd" "$p" 2>/dev/null | sha256sum)"
    h2="$("$DRIVER" "$cmd" "$p" 2>/dev/null | sha256sum)"
    [ "$h1" = "$h2" ] || { echo "MISMATCH: $cmd $f" >&2; exit 1; }
  done
done
rm -f "$A" "$B"
echo "SELF-REBUILD VERIFIED: driver -> shim -> driver, reproducible."
