# Build and gate both Windows archives — the Windows half of `./publish-private.sh`, run on
# the CI box by ci/windows/remote.ps1, which leaves `dist\<target>\` behind in the workspace
# for the publisher to fetch. The gate is build.yml's: build.sh's own verification, the
# workspace's tests against each archive, and the e2e binary run and checked for what it
# links.
#
# PowerShell 7. Installs nothing; the machine is provisioned by remotex's ci/windows/provision.ps1.
$ErrorActionPreference = 'Stop'
Set-Location (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$repo = (Get-Location).Path

# The developer environment: cl, link, cmake and the SDK's LIB and INCLUDE.
Import-Module 'C:\BuildTools\Common7\Tools\Microsoft.VisualStudio.DevShell.dll'
Enter-VsDevShell -VsInstallPath 'C:\BuildTools' -SkipAutomaticLocation -DevCmdArguments '-arch=x64 -host_arch=x64' | Out-Null
Set-Location $repo
# The MSYS2 shell inherits that environment: `inherit` keeps the Windows PATH in a login shell.
$env:MSYSTEM = 'MSYS'
$env:MSYS2_PATH_TYPE = 'inherit'
$env:CHERE_INVOKE = '1'

function Invoke-Step([string] $Name, [scriptblock] $Body) {
    Write-Host ''
    Write-Host "== $Name =="
    & $Body
    if ($LASTEXITCODE -ne 0) {
        Write-Host ''
        Write-Host "FAILED: $Name (exit $LASTEXITCODE)"
        exit $LASTEXITCODE
    }
}
function Invoke-Bash([string] $Script) {
    # A native path handed to cygpath inside the shell, so no path is spelled twice.
    & 'C:\msys64\usr\bin\bash.exe' -lc ('cd "$(cygpath -u ''' + $repo + ''')" && ' + $Script)
}

Write-Host '== toolchain =='
& cl 2>&1 | Select-Object -First 1
& cmake --version | Select-Object -First 1
& rustc --version
& cargo --version
if ($env:CARGO_TARGET_DIR) { Write-Host "   CARGO_TARGET_DIR=$env:CARGO_TARGET_DIR" }
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repo 'target' }

Invoke-Step 'build.sh windows-x86_64-msvc' { Invoke-Bash './build.sh windows-x86_64-msvc' }
Invoke-Step 'build.sh windows-x86_64-msvc-v3' { Invoke-Bash './build.sh windows-x86_64-msvc-v3' }
Invoke-Step 'sync-prebuilt.sh' { Invoke-Bash './sync-prebuilt.sh' }

# One flavour at a time: which archive a build links is a cargo feature, so each is tested
# by the build that links it. cargo from PowerShell, not from the MSYS shell: there
# `/usr/bin/link` (coreutils) shadows MSVC's link.exe and every build script fails to link.
foreach ($flavour in @(@{ Name = 'baseline'; Target = 'windows-x86_64-msvc'; Args = @() }, @{ Name = 'x86-64-v3'; Target = 'windows-x86_64-msvc-v3'; Args = @('--features', 'fdk-aac-e2e/x86-64-v3') })) {
    $extra = $flavour.Args
    Invoke-Step "cargo test ($($flavour.Name))" { & cargo test --offline --workspace @extra }
    Invoke-Step "cargo build ($($flavour.Name))" { & cargo build --offline --release --workspace @extra }
    Invoke-Step "end to end ($($flavour.Name))" {
        & "$target\release\fdk-aac-e2e.exe"
        if ($LASTEXITCODE -eq 0) { Invoke-Bash ('./check-static.sh "$(cygpath -u ''' + "$target\release\fdk-aac-e2e.exe" + ''')" ' + $flavour.Target) }
    }
}

Write-Host ''
Write-Host 'all steps passed'
