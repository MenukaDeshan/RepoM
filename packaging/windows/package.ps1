<#
.SYNOPSIS
  Build and package RepoM for Windows (zero-dependency setup installer + portable zip).

.DESCRIPTION
  Compiles RepoM for x86_64-pc-windows-msvc with the C runtime statically linked
  (`-C target-feature=+crt-static`), so the resulting binary runs on a fresh Windows 10/11
  PC with zero external dependencies (no Visual C++ Redistributable, WebView2, or .NET needed).

  Produces in -OutDir (default: dist/release):
    repom-<version>-windows-x86_64-setup.exe      Inno Setup 6 installer wizard
    repom-<version>-windows-x86_64-portable.zip   Standalone portable zip

  Requirements:
    - Rust stable MSVC toolchain (x86_64-pc-windows-msvc)
    - Inno Setup 6 (`iscc.exe`, preinstalled on GitHub Actions `windows-latest` or
      install locally with `winget install JRSoftware.InnoSetup`)

.EXAMPLE
  pwsh packaging/windows/package.ps1
  pwsh packaging/windows/package.ps1 -Version 0.1.0 -SkipBuild
#>
param(
  [string] $Version = '',
  [string] $OutDir = '',
  [string] $Target = '',
  [switch] $SkipBuild
)

$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path

# Auto-detect the active Windows x64 Rust host target (msvc or gnu) when -Target is omitted.
if (-not $Target) {
  $hostLine = (rustc -vV 2>$null | Select-String '^host:\s*(.+)$')
  if ($hostLine -and $hostLine.Matches[0].Groups[1].Value -match 'windows') {
    $Target = $hostLine.Matches[0].Groups[1].Value.Trim()
  } else {
    $Target = 'x86_64-pc-windows-msvc'
  }
}

function Invoke-Native([string] $What, [scriptblock] $Block) {
  Write-Host "==> $What"
  & $Block
  if ($LASTEXITCODE -ne 0) {
    throw "$What failed with exit code $LASTEXITCODE"
  }
}

# Read version from Cargo.toml if not passed explicitly.
if (-not $Version) {
  $inPkg = $false
  foreach ($line in Get-Content (Join-Path $Root 'Cargo.toml')) {
    if ($line -match '^\s*\[') {
      $inPkg = ($line.Trim() -eq '[package]')
      continue
    }
    if ($inPkg -and $line -match '^\s*version\s*=\s*"([^"]+)"') {
      $Version = $Matches[1]
      break
    }
  }
}
if (-not $Version) {
  throw 'Could not determine version from Cargo.toml'
}

$Dist = if ($OutDir) { $OutDir } elseif ($env:DIST) { $env:DIST } else { Join-Path $Root 'dist\release' }
$TargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $Root 'target' }
New-Item -ItemType Directory -Force -Path $Dist | Out-Null
$Dist = (Resolve-Path $Dist).Path

Write-Host "Packaging RepoM $Version for Windows x86_64 ($Target)"

if (-not $SkipBuild) {
  if ($Target -like '*-msvc') {
    # Statically link the MSVC C runtime so target PCs never need VC++ Redistributable.
    $flagVar = 'CARGO_TARGET_' + ($Target.ToUpper() -replace '-', '_') + '_RUSTFLAGS'
    [Environment]::SetEnvironmentVariable($flagVar, '-C target-feature=+crt-static')
  }
  Invoke-Native "cargo build --release --target $Target" {
    cargo build --release --target $Target
  }
}

# Locate built repom.exe (supports target/<triple>/release or target/release with -SkipBuild).
$BinDir = Join-Path $TargetDir "$Target\release"
$ExePath = Join-Path $BinDir 'repom.exe'
if (-not (Test-Path $ExePath) -and $SkipBuild) {
  $BinDir = Join-Path $TargetDir 'release'
  $ExePath = Join-Path $BinDir 'repom.exe'
}
if (-not (Test-Path $ExePath)) {
  throw "Built executable not found at $ExePath"
}

# Verify PE header: x64 machine (0x8664) and Windows GUI subsystem (2, no flashing console window).
$bytes = [System.IO.File]::ReadAllBytes($ExePath)
$pe = [BitConverter]::ToInt32($bytes, 0x3C)
$machine = [BitConverter]::ToUInt16($bytes, $pe + 4)
$subsystem = [BitConverter]::ToUInt16($bytes, $pe + 0x5C)
if ($machine -ne 0x8664) {
  throw "Unexpected PE machine 0x$('{0:X}' -f $machine), expected 0x8664 (x64)"
}
if ($subsystem -ne 2) {
  throw "Unexpected PE subsystem $subsystem, expected 2 (Windows GUI)"
}
Write-Host "Verified PE header: x64 GUI subsystem ($ExePath)"

# Locate Inno Setup compiler (iscc.exe).
$Iscc = (Get-Command iscc.exe -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source)
if (-not $Iscc) {
  foreach ($candidate in @(
    'C:\Program Files (x86)\Inno Setup 6\ISCC.exe',
    'C:\Program Files\Inno Setup 6\ISCC.exe',
    (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe')
  )) {
    if (Test-Path $candidate) {
      $Iscc = $candidate
      break
    }
  }
}
if (-not $Iscc) {
  throw "Inno Setup 6 (iscc.exe) not found. Install it with: winget install JRSoftware.InnoSetup"
}

$IssPath = Join-Path $PSScriptRoot 'repom.iss'
Invoke-Native 'Compiling Windows setup installer (Inno Setup)' {
  & $Iscc /Q `
    "/DAppVersion=$Version" `
    "/DRepoRoot=$Root" `
    "/DBinDir=$BinDir" `
    "/DOutputDir=$Dist" `
    $IssPath
}

# Build portable zip archive.
$PortableName = "repom-$Version-windows-x86_64-portable"
$StageDir = Join-Path $TargetDir "windows-package\$PortableName"
Remove-Item -Recurse -Force $StageDir -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $StageDir | Out-Null
Copy-Item $ExePath $StageDir
Copy-Item (Join-Path $Root 'README.md') $StageDir
Copy-Item (Join-Path $Root 'LICENSE') $StageDir
Copy-Item (Join-Path $Root 'repom.example.toml') $StageDir

$ZipPath = Join-Path $Dist "$PortableName.zip"
Remove-Item -Force $ZipPath -ErrorAction SilentlyContinue
Compress-Archive -Path "$StageDir\*" -DestinationPath $ZipPath

$SetupPath = Join-Path $Dist "repom-$Version-windows-x86_64-setup.exe"
Get-Item $SetupPath, $ZipPath | Format-Table Name, Length -AutoSize
