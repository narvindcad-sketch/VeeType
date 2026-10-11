param(
    [Parameter(Mandatory = $true)][string]$Executable,
    [Parameter(Mandatory = $true)][string]$Model,
    [Parameter(Mandatory = $true)][string]$WaveFile,
    [ValidateSet('cpu', 'vulkan')][string]$Backend = 'cpu'
)

$ErrorActionPreference = 'Stop'
$wave = [IO.File]::ReadAllBytes((Resolve-Path -LiteralPath $WaveFile).Path)
if ([Text.Encoding]::ASCII.GetString($wave, 0, 4) -ne 'RIFF' -or
    [Text.Encoding]::ASCII.GetString($wave, 8, 4) -ne 'WAVE') {
    throw 'Expected a RIFF WAVE file.'
}
$pcm = $null
$formatValidated = $false
for ($offset = 12; $offset + 8 -le $wave.Length;) {
    $kind = [Text.Encoding]::ASCII.GetString($wave, $offset, 4)
    $size = [BitConverter]::ToUInt32($wave, $offset + 4)
    $start = $offset + 8
    if ($start + $size -gt $wave.Length) { throw 'Truncated WAV chunk.' }
    if ($kind -eq 'fmt ') {
        if ($size -lt 16 -or [BitConverter]::ToUInt16($wave, $start) -ne 1 -or
            [BitConverter]::ToUInt16($wave, $start + 2) -ne 1 -or
            [BitConverter]::ToUInt32($wave, $start + 4) -ne 16000 -or
            [BitConverter]::ToUInt16($wave, $start + 14) -ne 16) {
            throw 'Expected 16 kHz mono signed 16-bit PCM.'
        }
        $formatValidated = $true
    }
    if ($kind -eq 'data') {
        $pcm = New-Object byte[] $size
        [Array]::Copy($wave, $start, $pcm, 0, $size)
    }
    $offset = $start + $size + ($size % 2)
}
if (-not $formatValidated -or -not $pcm -or $pcm.Length % 2 -ne 0) {
    throw 'WAV format or data is missing.'
}
$sampleCount = [int]($pcm.Length / 2)
$audio = New-Object byte[] ($sampleCount * 4)
for ($i = 0; $i -lt $sampleCount; $i++) {
    $sample = [single]([BitConverter]::ToInt16($pcm, $i * 2) / 32768.0)
    [Array]::Copy([BitConverter]::GetBytes($sample), 0, $audio, $i * 4, 4)
}

function Read-Exact([IO.Stream]$Stream, [int]$Length) {
    $buffer = New-Object byte[] $Length
    $offset = 0
    while ($offset -lt $Length) {
        $task = $Stream.ReadAsync($buffer, $offset, $Length - $offset)
        if (-not $task.Wait(180000)) { throw 'Worker response timed out.' }
        $read = $task.Result
        if ($read -eq 0) { throw 'Worker exited or closed its response pipe.' }
        $offset += $read
    }
    return ,$buffer
}

function Read-Response([IO.Stream]$Stream) {
    $kind = (Read-Exact $Stream 1)[0]
    $length = [BitConverter]::ToUInt32((Read-Exact $Stream 4), 0)
    if ($length -gt 16777216) { throw 'Invalid worker response length.' }
    $text = [Text.Encoding]::UTF8.GetString((Read-Exact $Stream $length))
    return @{ Kind = $kind; Text = $text }
}

$info = [Diagnostics.ProcessStartInfo]::new()
$info.FileName = (Resolve-Path -LiteralPath $Executable).Path
$modelPath = (Resolve-Path -LiteralPath $Model).Path
$info.Arguments = '--parakeet-worker "' + $modelPath + '" ' + $Backend + ' 2'
$info.UseShellExecute = $false
$info.CreateNoWindow = $true
$info.RedirectStandardInput = $true
$info.RedirectStandardOutput = $true
$info.RedirectStandardError = $true
$process = [Diagnostics.Process]::new()
$process.StartInfo = $info
$timer = [Diagnostics.Stopwatch]::StartNew()
try {
    [void]$process.Start()
    $ready = Read-Response $process.StandardOutput.BaseStream
    if ($ready.Kind -ne 0 -or $ready.Text -ne 'parakeet') {
        throw "Model did not initialize as Parakeet: $($ready.Text)"
    }
    Write-Output "Model loaded: backend=$Backend elapsed_ms=$($timer.ElapsedMilliseconds)"
    for ($iteration = 1; $iteration -le 2; $iteration++) {
        $timer.Restart()
        $inputStream = $process.StandardInput.BaseStream
        $count = [BitConverter]::GetBytes([uint32]$sampleCount)
        $inputStream.Write($count, 0, 4)
        $inputStream.Write($audio, 0, $audio.Length)
        $inputStream.Flush()
        $response = Read-Response $process.StandardOutput.BaseStream
        if ($response.Kind -ne 1 -or $response.Text -notmatch 'ask not' -or
            $response.Text -notmatch 'country') {
            throw "Known speech sample was not recognized: $($response.Text)"
        }
        Write-Output "Inference $iteration passed: audio_seconds=$($sampleCount / 16000.0) elapsed_ms=$($timer.ElapsedMilliseconds)"
        Write-Output $response.Text
    }
    $process.StandardInput.Close()
    if (-not $process.WaitForExit(10000)) { throw 'Worker did not exit after parent closed its pipe.' }
    if ($process.ExitCode -ne 0) { throw "Worker exited with code $($process.ExitCode)." }
    Write-Output 'PASS: real speech, persistent-session reuse, and pipe-close cleanup.'
} finally {
    if ($process.Id -and -not $process.HasExited) {
        $process.Kill()
        $process.WaitForExit()
    }
    $process.Dispose()
}
