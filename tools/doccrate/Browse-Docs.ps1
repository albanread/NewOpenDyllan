<#
.SYNOPSIS
    Open the NewOpenDylan manual in DocCrate.
.DESCRIPTION
    Launches doc-crate.exe pointed at docs/manual so you can browse the
    hand-written language + compiler documentation interactively.
.EXAMPLE
    .\Browse-Docs.ps1
#>
param(
    [string]$DocsDir = (Join-Path $PSScriptRoot '..\..\docs\manual')
)
$ErrorActionPreference = 'Stop'
$exe  = Join-Path $PSScriptRoot 'doc-crate.exe'
$docs = (Resolve-Path $DocsDir).Path
if (-not (Test-Path $exe))  { throw "doc-crate.exe not found at $exe" }
if (-not (Test-Path $docs)) { throw "docs dir not found: $docs" }
Start-Process -FilePath $exe -ArgumentList @($docs)
Write-Output "DocCrate launched on $docs"
