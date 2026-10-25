#!/bin/sh
# Fetches the Beat This! model files that analysis finds beats with into
# models/, checking each against its pinned SHA-256. They aren't in the
# repository: the checkpoints they're made from carry no licence of their
# own yet (docs/analysis.md). Run it from anywhere; it does nothing for a
# file that's already there and right.
set -eu

cd "$(dirname "$0")/.."
mkdir -p models

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d ' ' -f 1
    else
        shasum -a 256 "$1" | cut -d ' ' -f 1
    fi
}

# fetch NAME URL SHA256
fetch() {
    if [ -f "models/$1" ] && [ "$(sha256 "models/$1")" = "$3" ]; then
        echo "models/$1 is there"
        return
    fi
    echo "fetching models/$1"
    curl -fsSL --retry 4 -o "models/$1.part" "$2"
    got=$(sha256 "models/$1.part")
    if [ "$got" != "$3" ]; then
        rm -f "models/$1.part"
        echo "models/$1: SHA-256 $got, not $3" >&2
        exit 1
    fi
    mv "models/$1.part" "models/$1"
}

# beat-this 1.1.0's mel spectrogram, from the commit it was published from.
fetch mel_spectrogram.onnx \
    https://raw.githubusercontent.com/danigb/beat-this-rs/1ae768e78f1ad83b0ed3886241dc29ffde853c40/models/mel_spectrogram.onnx \
    fdd59e65c515331308e4c8841edf99972deca646bdf6197744c2a5b7755e3de9
# The full network, Beat This!'s final0 checkpoint.
fetch beat_this.onnx \
    https://github.com/danigb/beat-this-rs/releases/download/model-large/beat_this.onnx \
    5f810debe53459b559127fb55bbad40035bb47cc567b20e501670f968c770f02
