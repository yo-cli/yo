Add-Type -AssemblyName presentationCore
$file = "D:\code\ydcode\yo\voice\last_tts.mp3"

if (Test-Path $file) {
    Write-Host "Playing audio: $file" -ForegroundColor Green

    $player = New-Object System.Windows.Media.MediaPlayer
    $player.Open($file)
    $player.Play()

    # Wait for audio to load
    Start-Sleep -Milliseconds 500

    # Get duration and wait
    while ($player.NaturalDuration.HasTimeSpan -eq $false) {
        Start-Sleep -Milliseconds 100
    }

    $duration = $player.NaturalDuration.TimeSpan.TotalSeconds
    Write-Host "Duration: $duration seconds" -ForegroundColor Cyan
    Start-Sleep -Seconds $duration

    $player.Stop()
    $player.Close()

    Write-Host "Playback completed" -ForegroundColor Green
} else {
    Write-Host "File not found: $file" -ForegroundColor Red
}
