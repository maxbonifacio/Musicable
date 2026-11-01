# Time-stretch spike

A research spike for Musicable's own time-stretch engine, asked for by Maxim on 1 November 2026. The question it answers: can an engine written from the published methods, using what an offline DAW knows (the whole file, its onsets), beat Rubber Band R3 and élastique by ear? The plan and the methods are in the docs repo, `research/own-time-stretch.md`.

Nothing here is part of Musicable's workspace or CI.

- `reference/`: renders Rubber Band R3 (offline) and Signalsmith Stretch through Musicable's own bindings at a fixed ratio, writes the onsets Musicable's analysis finds, and measures where attacks land against round(t × ratio). This tool calls R3 but nobody writing the new engine reads R3's code.
- The new engine is written clean-room, from papers only, by an agent that is given no Rubber Band, élastique or Bungee source or notes, and no access to this repository. Its sources are listed in its own README.

The listening material is Maxim's and stays out of the repository.
