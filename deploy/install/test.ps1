# Tests install.ps1 for real on Windows (docs/12 §1), like test.sh: install
# from a local release folder, start the program and send a first message;
# a corrupted archive installs nothing.
#
#   pwsh deploy/install/test.ps1 <release dir with agentim-x86_64-pc-windows-msvc.zip[.sha256]>
param([Parameter(Mandatory)] [string] $Dist)
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$root = (Resolve-Path (Join-Path $here '..\..')).Path
$Dist = (Resolve-Path $Dist).Path
$work = Join-Path ([System.IO.Path]::GetTempPath()) ("agentim-test-" + [System.Guid]::NewGuid())
New-Item -ItemType Directory -Path $work | Out-Null

function Serve($dir) {
    $l = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $l.Start(); $port = $l.LocalEndpoint.Port; $l.Stop()
    $p = Start-Process python -ArgumentList '-m', 'http.server', $port, '--bind', '127.0.0.1', '--directory', $dir -PassThru -WindowStyle Hidden
    foreach ($i in 1..50) {
        try { Invoke-WebRequest -UseBasicParsing "http://127.0.0.1:$port/" | Out-Null; break } catch { Start-Sleep -Milliseconds 100 }
    }
    return @{ Process = $p; Url = "http://127.0.0.1:$port" }
}

function Install($url, $dir) {
    $env:AGENTIM_DOWNLOAD_URL = $url
    $env:AGENTIM_INSTALL_DIR = $dir
    $env:AGENTIM_NO_MODIFY_PATH = '1'
    $out = & pwsh -NoProfile -File (Join-Path $here 'install.ps1') *>&1 | Out-String
    Write-Host $out
    return $LASTEXITCODE
}

$s = Serve $Dist
try {
    Write-Host '== install'
    $dir = Join-Path $work 'bin'
    $code = Install $s.Url $dir
    if ($code -ne 0) { throw "install failed ($code)" }
    $exe = Join-Path $dir 'agentim.exe'
    & $exe --version
    if ($LASTEXITCODE -ne 0) { throw 'the installed program does not run' }
    python (Join-Path $root 'deploy\smoke\first_message.py') $exe
    if ($LASTEXITCODE -ne 0) { throw 'first message failed' }

    Write-Host '== install again (an update replaces the program)'
    if ((Install $s.Url $dir) -ne 0) { throw 'second install failed' }
    if (-not (Test-Path $exe)) { throw 'the program is gone after an update' }
} finally {
    Stop-Process -Id $s.Process.Id -Force -ErrorAction SilentlyContinue
}

Write-Host '== a corrupted archive installs nothing'
$bad = Join-Path $work 'bad'
New-Item -ItemType Directory -Path $bad | Out-Null
Copy-Item (Join-Path $Dist '*') $bad
Get-ChildItem $bad -Filter *.zip | ForEach-Object { Add-Content -Path $_.FullName -Value 'x' -NoNewline }
$s = Serve $bad
try {
    $fresh = Join-Path $work 'fresh'
    $env:AGENTIM_DOWNLOAD_URL = $s.Url
    $env:AGENTIM_INSTALL_DIR = $fresh
    $out = & pwsh -NoProfile -File (Join-Path $here 'install.ps1') *>&1 | Out-String
    if ($LASTEXITCODE -eq 0) { throw "a corrupted archive was installed: $out" }
    if ($out -notmatch 'checksum mismatch') { throw "unexpected error: $out" }
    if (Test-Path (Join-Path $fresh 'agentim.exe')) { throw 'something was installed' }
} finally {
    Stop-Process -Id $s.Process.Id -Force -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
Write-Host 'install.ps1: all checks passed'
# $LASTEXITCODE still holds the refused install above.
exit 0
