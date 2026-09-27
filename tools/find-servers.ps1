# Asks the network for OpenVideoServer servers, without the client.
#
#   powershell -ExecutionPolicy Bypass -File tools\find-servers.ps1
#
# The client's connect screen can say it found nothing, and since 2026-09-27 it
# can say why. This answers the question one layer down: can this machine ask
# the network at all, and does anything answer. It uses nothing but .NET, so it
# runs on a machine that has never had this repository on it.
#
# It reports per adapter, which is the point of it. A laptop with a VPN, Hyper-V
# or WSL on it has three or four addresses, and a multicast query leaves by
# whichever one the routing table prefers unless it is told otherwise, so an
# adapter that works and an adapter that does not look identical from a single
# combined answer.

$ErrorActionPreference = 'Stop'

$service = '_openvideoserver._tcp.local'
$group = [System.Net.IPAddress]::Parse('224.0.0.251')
$port = 5353

# A standard PTR question for the service name, built by hand: id 0, one
# question, class IN. The same packet the client sends.
function New-Query([string]$name) {
    $bytes = New-Object System.Collections.Generic.List[byte]
    0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0 | ForEach-Object { $bytes.Add([byte]$_) }
    foreach ($label in $name.Split('.')) {
        if ($label.Length -eq 0) { continue }
        $bytes.Add([byte]$label.Length)
        $bytes.AddRange([System.Text.Encoding]::ASCII.GetBytes($label))
    }
    $bytes.Add([byte]0)
    # One at a time: `AddRange` on a `List[byte]` will not take a PowerShell
    # object array, and the cast it wants is not worth the line it costs.
    $bytes.Add([byte]0); $bytes.Add([byte]12)  # PTR
    $bytes.Add([byte]0); $bytes.Add([byte]1)   # IN
    return $bytes.ToArray()
}

$query = New-Query $service

$addresses = @(
    [System.Net.NetworkInformation.NetworkInterface]::GetAllNetworkInterfaces() |
        Where-Object { $_.OperationalStatus -eq 'Up' -and $_.NetworkInterfaceType -ne 'Loopback' } |
        ForEach-Object {
            $name = $_.Name
            $_.GetIPProperties().UnicastAddresses |
                Where-Object { $_.Address.AddressFamily -eq 'InterNetwork' } |
                ForEach-Object { [pscustomobject]@{ Adapter = $name; Address = $_.Address } }
        }
)

if ($addresses.Count -eq 0) {
    Write-Host 'This machine has no IPv4 adapter that is up. Nothing to ask from.'
    exit 1
}

Write-Host ("asking on {0} adapter(s), 4 seconds each" -f $addresses.Count)
Write-Host ''

$total = 0
foreach ($entry in $addresses) {
    $client = $null
    try {
        # Bound to this adapter's own address, so the question leaves by this
        # adapter and no other. Port 0: an answer to a question from an
        # ephemeral port comes back to it, which is what makes this work
        # alongside a responder already holding 5353.
        $client = New-Object System.Net.Sockets.UdpClient(
            (New-Object System.Net.IPEndPoint($entry.Address, 0)))
        $client.MulticastLoopback = $false
        $client.Client.ReceiveTimeout = 4000
        $client.Send($query, $query.Length,
            (New-Object System.Net.IPEndPoint($group, $port))) | Out-Null

        $heard = 0
        $named = @()
        $until = (Get-Date).AddSeconds(4)
        while ((Get-Date) -lt $until) {
            try {
                $from = New-Object System.Net.IPEndPoint([System.Net.IPAddress]::Any, 0)
                $data = $client.Receive([ref]$from)
                $heard++
                $text = [System.Text.Encoding]::ASCII.GetString($data)
                if ($text -match 'openvideoserver') {
                    $named += $from.Address.ToString()
                }
            } catch [System.Net.Sockets.SocketException] {
                break
            }
        }

        $total += $named.Count
        if ($named.Count -gt 0) {
            Write-Host ("  {0} ({1}): answered by {2}" -f `
                $entry.Adapter, $entry.Address, (($named | Select-Object -Unique) -join ', ')) `
                -ForegroundColor Green
        } elseif ($heard -gt 0) {
            Write-Host ("  {0} ({1}): heard {2} packet(s), none from a server" -f `
                $entry.Adapter, $entry.Address, $heard) -ForegroundColor Yellow
        } else {
            Write-Host ("  {0} ({1}): silence, not one packet" -f `
                $entry.Adapter, $entry.Address) -ForegroundColor Red
        }
    } catch {
        Write-Host ("  {0} ({1}): could not ask - {2}" -f `
            $entry.Adapter, $entry.Address, $_.Exception.Message) -ForegroundColor Red
    } finally {
        if ($null -ne $client) { $client.Close() }
    }
}

Write-Host ''
if ($total -gt 0) {
    Write-Host 'A server answered. Discovery can work from this machine.'
} else {
    Write-Host 'No server answered on any adapter.'
    Write-Host 'Silence on every one means this machine is not hearing the network.'
    Write-Host 'Packets but no server means the server is not on the segment this machine is on.'
}
