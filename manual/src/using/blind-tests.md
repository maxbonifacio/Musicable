# Blind tests

A blind test plays you pairs of sounds without saying which is which, so
what you hear decides, not what you know. It's for checking Musicable
against something else: a warp mode against another app's, or a built-in
device against the hardware it's modelled on.

## Making a round

A round is a folder of pairs. An agent makes one with **Make Blind Test**
(`debug.make_blind_test`), from two lists of files of the same length,
each file in the first paired with the one at its place in the second,
such as Live's renders and Musicable's. Match their loudness first, with
[`musicable audio match`](../reference/audio-commands.md), so the louder one doesn't
win for being louder.

Each pair becomes a trial, in a shuffled order. Its two files are copied
into the folder as `01 A` and `01 B`, which is which chosen at random, and
in an ABX round, one of them is copied again as `01 X`. The folder's
`key.json` says where each copy came from: leave it closed until you've
judged.

## Judging

**Debug > Open Blind Test...** asks for the round's folder and opens the
Blind Test window, at the first trial still to judge.

- **Play A**, **Play B** and **Play X** play the trial's sounds through
  your audio device, as the [browser's previews](../window/browser.md#previews)
  do, after the master and as loud as the browser's preview slider says.
  Clicking the one playing stops it, as **Stop** does.
- With **Switch at the same place** on, the next sound starts where the
  one playing was, so you can flip between them mid-phrase.
- **X is** A or B, in an ABX round, says which one X is a copy of.
- **Better** says which you prefer, A, B, or the **Same**, and **Why**
  keeps what you heard.
- **Previous** and **Next** move between the trials.

Each answer is kept in the folder's `answers.json` as you give it, so you
can close the window and judge the rest later.

## Unsealing

Once every trial is judged, **Unseal** reads the key and writes
`results.json`, and the window says how it went:

- In an ABX round, how many X's you named right, and the chance of doing
  as well by guessing. Under 0.05 is the usual bar for hearing a
  difference.
- How often you preferred each list, by the names the round was made with,
  and the sign test's chance of a split as lopsided by tossing a coin.
- For each trial, which file A and B were and whether you named X right.

After unsealing, the answers can't be changed: the results stand as you
judged them.
