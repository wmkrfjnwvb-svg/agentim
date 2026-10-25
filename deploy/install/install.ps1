# Installs the agentim server for this user on Windows (docs/12 §1):
# downloads the build from GitHub Releases, checks its SHA-256 and puts it
# in ~\.agentim\bin, which is added to the user's PATH. No administrator
# rights needed. Nothing is installed when the checksum does not match.
#
#   irm https://github.com/<owner>/agentim/releases/latest/download/install.ps1 | iex
#
# Settings (environment): AGENTIM_VERSION, AGENTIM_INSTALL_DIR,
# AGENTIM_DOWNLOAD_URL, AGENTIM_REPO (as in install.sh), and
# AGENTIM_NO_MODIFY_PATH=1 to leave PATH alone.

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

# Thrown, not `exit`: run through `iex`, exit would close the user's window.
function Fail($msg) {
    throw "agentim-install: error: $msg"
}

$repo = if ($env:AGENTIM_REPO) { $env:AGENTIM_REPO } else { '__AGENTIM_REPO__' }
$version = if ($env:AGENTIM_VERSION) { $env:AGENTIM_VERSION } else { 'latest' }
$dir = if ($env:AGENTIM_INSTALL_DIR) { $env:AGENTIM_INSTALL_DIR } else { Join-Path $HOME '.agentim\bin' }
$base = $env:AGENTIM_DOWNLOAD_URL
if (-not $base) {
    if ($repo -notmatch '^[^/]+/[^/]+$') { Fail 'set AGENTIM_REPO=<owner>/<name> or AGENTIM_DOWNLOAD_URL' }
    if ($version -eq 'latest') {
        $base = "https://github.com/$repo/releases/latest/download"
    } else {
        if (-not $version.StartsWith('v')) { $version = "v$version" }
        $base = "https://github.com/$repo/releases/download/$version"
    }
}

# x64 builds also run on Windows on Arm (emulation).
$archive = 'agentim-x86_64-pc-windows-msvc.zip'
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("agentim-" + [System.Guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    Write-Host "agentim-install: downloading $base/$archive"
    try {
        Invoke-WebRequest -UseBasicParsing -Uri "$base/$archive" -OutFile (Join-Path $tmp $archive)
        Invoke-WebRequest -UseBasicParsing -Uri "$base/$archive.sha256" -OutFile (Join-Path $tmp "$archive.sha256")
    } catch {
        Fail "could not download from ${base}: $($_.Exception.Message)"
    }
    $want = ((Get-Content (Join-Path $tmp "$archive.sha256") -Raw).Trim() -split '\s+')[0].ToLowerInvariant()
    if ($want -notmatch '^[0-9a-f]{64}$') { Fail 'the checksum file is not a SHA-256' }
    $got = (Get-FileHash -Algorithm SHA256 (Join-Path $tmp $archive)).Hash.ToLowerInvariant()
    if ($got -ne $want) { Fail "checksum mismatch for $archive (expected $want, got $got); nothing was installed" }

    $x = Join-Path $tmp 'x'
    Expand-Archive -Path (Join-Path $tmp $archive) -DestinationPath $x
    $exe = Join-Path $x 'agentim.exe'
    if (-not (Test-Path $exe)) { Fail "$archive does not contain agentim.exe" }
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $target = Join-Path $dir 'agentim.exe'
    # A running agentim.exe cannot be overwritten, but it can be renamed.
    if (Test-Path $target) {
        $old = "$target.old"
        Remove-Item -Force $old -ErrorAction SilentlyContinue
        Rename-Item -Path $target -NewName 'agentim.exe.old'
    }
    Copy-Item $exe $target
    $v = & $target --version
    if ($LASTEXITCODE -ne 0) { Fail 'the installed program does not run on this machine' }
    Write-Host "agentim-install: installed $v to $target"

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $parts = if ($userPath) { $userPath -split ';' } else { @() }
    if ($parts -notcontains $dir) {
        if ($env:AGENTIM_NO_MODIFY_PATH -eq '1') {
            Write-Host "agentim-install: $dir is not on your PATH; run $target"
        } else {
            [Environment]::SetEnvironmentVariable('Path', (($parts + $dir) -join ';').Trim(';'), 'User')
            Write-Host "agentim-install: added $dir to your PATH (open a new terminal), then run: agentim"
        }
    } else {
        Write-Host 'agentim-install: run: agentim'
    }
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
