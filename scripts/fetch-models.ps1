# Fetches the Beat This! model files that analysis finds beats with into
# models\, checking each against its pinned SHA-256, as fetch-models.sh does
# (which says why they aren't in the repository). Run it from anywhere; it
# does nothing for a file that's already there and right.
$ErrorActionPreference = 'Stop'

$models = Join-Path (Split-Path $PSScriptRoot -Parent) 'models'
New-Item -ItemType Directory -Force -Path $models | Out-Null

function Fetch($name, $url, $sha256) {
    $path = Join-Path $models $name
    if ((Test-Path $path) -and ((Get-FileHash -Algorithm SHA256 $path).Hash -eq $sha256)) {
        Write-Host "models\$name is there"
        return
    }
    Write-Host "fetching models\$name"
    $part = "$path.part"
    Invoke-WebRequest -Uri $url -OutFile $part -UseBasicParsing
    $got = (Get-FileHash -Algorithm SHA256 $part).Hash
    if ($got -ne $sha256) {
        Remove-Item $part
        throw "models\${name}: SHA-256 $got, not $sha256"
    }
    Move-Item -Force $part $path
}

# beat-this 1.1.0's mel spectrogram, from the commit it was published from.
Fetch 'mel_spectrogram.onnx' `
    'https://raw.githubusercontent.com/danigb/beat-this-rs/1ae768e78f1ad83b0ed3886241dc29ffde853c40/models/mel_spectrogram.onnx' `
    'FDD59E65C515331308E4C8841EDF99972DECA646BDF6197744C2A5B7755E3DE9'
# The full network, Beat This!'s final0 checkpoint.
Fetch 'beat_this.onnx' `
    'https://github.com/danigb/beat-this-rs/releases/download/model-large/beat_this.onnx' `
    '5F810DEBE53459B559127FB55BBAD40035BB47CC567B20E501670F968C770F02'
