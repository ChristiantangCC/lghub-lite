# Dev-only, read-only G HUB agent client. Not part of the crate.
# Usage: powershell -File tools/agent.ps1 /devices/list [outfile]
# Only GET is allowed; the agent is a research oracle, never a dependency.
param([Parameter(Mandatory)][string]$Path, [string]$Out)

$ws = New-Object System.Net.WebSockets.ClientWebSocket
$ws.Options.AddSubProtocol('json')   # no Origin header is sent (protocol.md §9.11)
$ct = [Threading.CancellationToken]::None
$ws.ConnectAsync([Uri]'ws://127.0.0.1:9010', $ct).Wait()

$req = '{"msgId":"q1","verb":"GET","path":"' + $Path + '"}'
$b = [Text.Encoding]::UTF8.GetBytes($req)
$ws.SendAsync([ArraySegment[byte]]$b, 'Text', $true, $ct).Wait()

$buf = New-Object byte[] 1048576
$reply = $null
for ($i = 0; $i -lt 50 -and -not $reply; $i++) {
    $ms = New-Object IO.MemoryStream
    do {
        $t = $ws.ReceiveAsync([ArraySegment[byte]]$buf, $ct)
        if (-not $t.Wait(10000)) { break }
        $r = $t.Result
        $ms.Write($buf, 0, $r.Count)
    } while (-not $r.EndOfMessage)
    $m = [Text.Encoding]::UTF8.GetString($ms.ToArray())
    if ($m -match '"msgId":\s*"q1"') { $reply = $m }
}
$ws.Dispose()
if (-not $reply) { Write-Error "no reply"; exit 1 }
if ($Out) { [IO.File]::WriteAllText($Out, $reply) } else { $reply }
