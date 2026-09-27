# Runs the desktop client's own session code, here, against a real `ovsd` there.
#
#   .\tools\client-live.ps1                  against $env:OVS_REMOTE
#   .\tools\client-live.ps1 -Remote ovs-test
#   .\tools\client-live.ps1 -Port 7460       if 7450 is busy over there
#
# The client is a Windows application. It is tested on Windows — this runs
# `clients/desktop/test/live.test.ts` in this working tree, on this machine,
# with Node and the packages that are already here. Nothing of the client goes
# near the server: the only thing that runs there is `ovsd` itself, which is
# what a server is for.
#
# # What it does
#
# Sends the working tree to the remote machine and builds `ovsd` (Windows
# cannot build it: the decoder, the encoder and the media probe are behind
# `cfg(unix)`), starts a throwaway one on a spare port with an empty database
# under a temporary directory, runs the suite here against it, and stops it.
#
# The throwaway server is deaf and blind by construction: its own port, its own
# database, no machine configuration, no Art-Net or sACN socket, and a renderer
# address with nothing behind it. It cannot reach the renderer that is driving
# a wall, and it stops itself after fifteen minutes even if this script never
# gets the chance to.
#
# # Why it is worth running
#
# This is the only place a contract the client and the server read differently
# is caught. Everything else tests one half against this repository's own idea
# of the other. The first time it ran it found three real faults.

[CmdletBinding()]
param(
  [string]$Remote = $env:OVS_REMOTE,
  [int]$Port = 7450,
  [string]$User = 'operator',
  [string]$Password = 'correct horse battery staple'
)

$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent $PSScriptRoot)

if ([string]::IsNullOrWhiteSpace($Remote)) {
  Write-Error 'name the ssh host: -Remote ovs-test, or set OVS_REMOTE'
}

# Where the tree lives on the far side, matching tools/remote.sh.
$target = if ($env:OVS_REMOTE_DIR) { $env:OVS_REMOTE_DIR } else { 'openvideoserver' }

# The address the suite will be pointed at. Resolved from the ssh host, because
# that is the name already known to work for reaching this machine.
$hostName = (& ssh -o BatchMode=yes $Remote 'hostname -I' 2>$null)
if ($LASTEXITCODE -ne 0) { Write-Error "cannot reach $Remote over ssh" }
$serverIp = ($hostName -split '\s+' | Where-Object { $_ -match '^\d+\.' } | Select-Object -First 1)
if (-not $serverIp) { Write-Error "$Remote did not say what address it has" }

# Git's own bash, found through git rather than through PATH.
#
# Two things went wrong before this did: PowerShell will not run a .sh by path
# and does not say so — it leaves $LASTEXITCODE as whatever it already was, so
# a build that never happened reported success and an hour-old tree was tested.
# And `bash` on PATH here is Windows' WSL stub, which has its own filesystem
# and its own ssh config, and could neither read D:\ nor resolve the host.
$gitBash = $null
$git = Get-Command git -ErrorAction SilentlyContinue
if ($git) {
  $gitRoot = Split-Path -Parent (Split-Path -Parent $git.Source)
  $candidate = Join-Path $gitRoot 'bin/bash.exe'
  if (Test-Path $candidate) { $gitBash = $candidate }
}
if (-not $gitBash) {
  Write-Error 'this needs the bash that comes with Git for Windows, and git is not on PATH'
}

Write-Host "sending the tree to $Remote and building ovsd"
# A relative path, because bash on Windows reads a backslash in an argument as
# an escape; and the host named here rather than inherited, because -Remote is
# allowed to differ from whatever OVS_REMOTE happens to hold.
& $gitBash -c "OVS_REMOTE='$Remote' tools/remote.sh -- 'cargo build --quiet -p ovs-core --bin ovsd'"
if ($LASTEXITCODE -ne 0) { Write-Error 'ovsd did not build' }

$remoteJob = $null
try {
  Write-Host "starting a throwaway ovsd on ${serverIp}:$Port"
  # Started as its own job so that this script keeps its own output, and so
  # that stopping it is stopping one thing. `-t -t` gives the remote side a
  # terminal, which is what makes ssh take the process down with it when this
  # end goes away — the belt to the script's own deadline braces.
  $remoteJob = Start-Job -ScriptBlock {
    param($remote, $target, $port, $user, $password)
    & ssh -o BatchMode=yes -t -t $remote `
      "export PATH=`$HOME/.cargo/bin:`$PATH; cd '$target' && OVS_LIVE_PORT=$port OVS_LIVE_USER='$user' OVS_LIVE_PASSWORD='$password' bash tools/client-live.sh --serve" 2>&1
  } -ArgumentList $Remote, $target, $Port, $User, $Password

  $base = "http://${serverIp}:$Port/api/v1"
  Write-Host "waiting for $base to answer"
  $answered = $false
  foreach ($attempt in 1..90) {
    if ($remoteJob.State -eq 'Completed' -or $remoteJob.State -eq 'Failed') {
      Receive-Job $remoteJob | Write-Host
      Write-Error 'the throwaway server stopped before it could answer'
    }
    try {
      $null = Invoke-RestMethod -Method Get -Uri "$base/server/capabilities" -TimeoutSec 2
      # Answering is not enough: the account is made after the socket opens, and
      # a suite that signed in before it existed would fail about the wrong
      # thing. The script says READY once both are true.
      $said = (Receive-Job $remoteJob -Keep) -join "`n"
      if ($said -match 'READY ') { $answered = $true; break }
    } catch {
      # A refused connection is what waiting looks like for the first second.
    }
    Start-Sleep -Milliseconds 500
  }
  if (-not $answered) {
    Receive-Job $remoteJob | Write-Host
    Write-Error "$base never became ready"
  }

  Write-Host "running the client's live suite here, against it"
  Push-Location clients/desktop
  try {
    $env:OVS_LIVE_SERVER = "${serverIp}:$Port"
    $env:OVS_LIVE_USER = $User
    $env:OVS_LIVE_PASSWORD = $Password
    # The FFmpeg and ffprobe the client ships with, which are the ones it uses
    # in an operator's hands. `prepare-live.test.ts` skips itself without them,
    # and a preparation suite that silently skipped on the machine that does
    # the preparing would be worth nothing.
    #
    # Named here rather than found by the client's own `locate`, because that
    # looks beside the built `main.cjs` and under vitest the code is running
    # from `src/`. Both, because probing and encoding are two binaries and
    # setting only one leaves the suite failing about the other.
    $ffmpeg = Join-Path (Get-Location) 'dist/bin/ffmpeg.exe'
    $ffprobe = Join-Path (Get-Location) 'dist/bin/ffprobe.exe'
    if ((Test-Path $ffmpeg) -and (Test-Path $ffprobe)) {
      $env:OVS_FFMPEG = $ffmpeg
      $env:OVS_FFPROBE = $ffprobe
    } else {
      Write-Host 'no bundled FFmpeg in dist/bin; the preparation suite will skip itself'
      Write-Host 'run npm run client:build first if you want it'
    }
    & npx vitest run live
    $status = $LASTEXITCODE
  } finally {
    Remove-Item Env:OVS_LIVE_SERVER, Env:OVS_LIVE_USER, Env:OVS_LIVE_PASSWORD, Env:OVS_FFMPEG, Env:OVS_FFPROBE -ErrorAction SilentlyContinue
    Pop-Location
  }

  if ($status -ne 0) {
    Write-Host 'the suite failed; what the server said:'
    Receive-Job $remoteJob | Write-Host
  }
  exit $status
} finally {
  if ($remoteJob) {
    # Ending the ssh session ends the remote script, whose own trap deletes the
    # database and stops the server. The deadline in that script is what covers
    # the case where even this does not happen.
    Stop-Job $remoteJob -ErrorAction SilentlyContinue
    Remove-Job $remoteJob -Force -ErrorAction SilentlyContinue
  }
}
