# Time-stretch spike

A research spike for Musicable's own time-stretch engine, asked for by Maxim on 1 November 2026. The question it answers: can an engine written from the published methods, using what an offline DAW knows (the whole file, its onsets), beat Rubber Band R3 and élastique by ear? The plan and the methods are in the docs repo, `research/own-time-stretch.md`.

Nothing here is part of Musicable's workspace or CI.

- `reference/`: renders Rubber Band R3 (offline) and Signalsmith Stretch through Musicable's own bindings at a fixed ratio, writes the onsets Musicable's analysis finds, and measures where attacks land against round(t × ratio). This tool calls R3 but nobody writing the new engine reads R3's code.
- `engine/`: the new engine, tsm-lab, written clean-room from papers only, by an agent that was given no Rubber Band, élastique or Bungee source or notes, and no access to this repository. Its README has the design, the self-checks and the clean-room record. Changes to it go through the same kind of agent, never one that has read R3.

## Making a listening round

```
cargo build --release --manifest-path spikes/time-stretch/engine/Cargo.toml
cargo build --release --manifest-path spikes/time-stretch/reference/Cargo.toml
stretch-reference prepare --material <folder of WAVs> --out <new folder> \
    --engine <tsm-lab> --musicable <musicable> [--ratios 0.8,1.25,2]
```

`prepare` finds each file's onsets, renders it with R3, Signalsmith and the engine's `full`, `pv` and `hptsm` variants at each ratio, moves each render by its measured attack lag, and brings every render of a file and ratio to -23 LUFS with `musicable audio match`. `matched/` then holds the files to listen to, `timing.tsv` where the attacks landed, and `round.json` the two lists (ours first, R3 second) to give `debug.make_blind_test`.

The listening material is Maxim's and stays out of the repository.
