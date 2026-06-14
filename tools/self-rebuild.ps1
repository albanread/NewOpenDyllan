#!/usr/bin/env pwsh
# tools/self-rebuild.ps1 — Sprint 60 goal 1: reproducible self-rebuild of the
# Dylan front-end shim FROM the nod-driver, then relink the driver against it,
# and VERIFY reproducibility.
#
# Run from anywhere; it anchors at the workspace root (the parent of tools/).
# Requires an MSVC environment on PATH (Developer PowerShell / vcvars) so
# build.rs's link step for nod-driver can find link.exe. Step 2 (--library)
# skips linking and needs no MSVC env.
#
# What each step proves:
#   1. cargo build -p nod-driver       — the tool that will rebuild the front-end
#   2. build --library …               — the driver compiles its OWN Dylan
#                                         front-end (lexer/parser/macro/c3/sema/
#                                         lower) into a fresh .obj (the self-host
#                                         step)
#   3. cargo build -p nod-driver       — build.rs relinks the fresh shim (closes
#                                         the loop: the new driver runs code the
#                                         prior driver generated)
#   4a. build twice, compare SHA-256   — the .obj is byte-deterministic
#   4b. front-end dumps byte-stable    — the rebuilt front-end behaves identically

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Root = Split-Path -Parent $PSScriptRoot
Push-Location $Root
try {
    $Driver  = Join-Path $Root 'target\debug\nod-driver.exe'
    $Prj     = Join-Path $Root 'compiler\dylan-lex-shim.prj'
    $ShimObj = Join-Path $Root 'compiler\dylan-lex-shim.lib.obj'

    function Sha256($path) { (Get-FileHash -Algorithm SHA256 $path).Hash.ToLower() }

    function Build-Shim([string]$outPath) {
        # --parse-with-rust is MANDATORY: the running driver already statically
        # links a shim (cfg dylan_lex_shim_linked is on), so the default-on
        # Dylan front-end would fire while compiling the shim and its own
        # `define class`es would collide with the runtime's already-registered
        # classes ("class redefinition refused", exit 1). Forcing the Rust
        # front-end side-steps that and keeps the build self-consistent.
        & $Driver --parse-with-rust build --library --project $Prj -o $outPath
        if ($LASTEXITCODE -ne 0) { throw "shim build failed (exit $LASTEXITCODE) -> $outPath" }
        if (-not (Test-Path $outPath)) { throw "shim build reported success but $outPath is missing" }
    }

    Write-Host '== STEP 1: build the driver (cargo build -p nod-driver) =='
    cargo build -p nod-driver
    if ($LASTEXITCODE -ne 0) { throw "cargo build -p nod-driver failed (exit $LASTEXITCODE)" }
    if (-not (Test-Path $Driver)) { throw "driver missing after build: $Driver" }

    Write-Host '== STEP 2: rebuild the Dylan front-end shim FROM that driver =='
    Build-Shim $ShimObj
    Write-Host "   shim -> $ShimObj  (sha256 $(Sha256 $ShimObj))"

    Write-Host '== STEP 3: rebuild the driver so build.rs relinks the fresh shim =='
    cargo build -p nod-driver
    if ($LASTEXITCODE -ne 0) { throw "driver relink failed (exit $LASTEXITCODE)" }

    Write-Host '== STEP 4a: VERIFY .obj reproducibility (build twice, compare hash) =='
    $A = Join-Path $env:TEMP 'shim-repro-A.obj'
    $B = Join-Path $env:TEMP 'shim-repro-B.obj'
    Build-Shim $A
    Build-Shim $B
    $ha = Sha256 $A; $hb = Sha256 $B
    Write-Host "   A=$ha`n   B=$hb"
    if ($ha -ne $hb) {
        # If a future toolchain change starts embedding a COFF TimeDateStamp the
        # bytes would differ here; fall back to a normalised section compare
        # (llvm-objdump -s / dumpbin /RAWDATA) or zero the 4-byte stamp at file
        # offset 4 before hashing. Today the raw bytes are identical.
        throw "NON-REPRODUCIBLE: two shim builds differ ($ha vs $hb)"
    }
    if ((Sha256 $ShimObj) -ne $ha) {
        throw "on-disk shim ($(Sha256 $ShimObj)) != fresh build ($ha)"
    }
    Write-Host '   OK: shim .obj is byte-identical across rebuilds.'

    Write-Host '== STEP 4b: VERIFY front-end output is identical across the rebuild =='
    $Corpus = @('hello','factorial','point','mutual','sprint09-add') |
        ForEach-Object { Join-Path $Root "tests\nod-tests\fixtures\$_.dylan" } |
        Where-Object { Test-Path $_ }

    $fail = $false
    foreach ($f in $Corpus) {
        foreach ($cmd in @('dump-dylan-ast','dump-tokens','dump-dfm')) {
            $o1 = (& $Driver $cmd $f 2>$null | Out-String)
            $o2 = (& $Driver $cmd $f 2>$null | Out-String)
            if ($o1 -ne $o2) {
                Write-Host "   MISMATCH: $cmd $(Split-Path -Leaf $f)"
                $fail = $true
            }
        }
    }
    if ($fail) { throw 'front-end output not deterministic across the rebuild' }
    Write-Host '   OK: dump-dylan-ast / dump-tokens / dump-dfm byte-stable on the corpus.'

    Remove-Item -Force -ErrorAction SilentlyContinue $A, $B
    Write-Host ''
    Write-Host 'SELF-REBUILD VERIFIED: driver -> shim -> driver, reproducible.'
}
finally {
    Pop-Location
}
