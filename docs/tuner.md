# The blind A/B tuner

On top of the factory tuning, a 5-band **Personal EQ** applies to every profile: bass
shelf, low-mid, mid, presence and treble shelf. You can set it by hand in the TUI, or let
the tuner find it with you.

## Running a session

1. Open **Personal EQ**, select **▶ Find my EQ: blind A/B test** at the top, and press
   Enter (or click it).
2. Choose the music:
   - one of three bundled excerpts: electronic, a band with vocals, or funk;
   - or any song under `~/Music`. neiro plays its fullest-sounding 10 seconds, not the intro.
3. For each trial, listen to **A** and **B** and pick the better one:

   | Key | Action |
   |---|---|
   | `a` / `b` or `space` | Switch what you hear |
   | `1` / `2` | Choose A or B |
   | `0` | You can't tell a difference |
   | `x` | Alternate A and B automatically every 4 s |
   | `Esc` | Finish early |

4. After about 20–30 answers you see the resulting curve. Keep it as your Personal EQ or
   discard it.

Pause other audio while tuning.

## Why you can trust the result

- **Blind:** which side holds the change is random, and the screen never says what changed.
- **Loudness-matched:** both sides are set to the same K-weighted loudness, as in LUFS.
  The louder one can't win just for being louder.
- **Coarse to fine:**
  1. each band's gain ±3 dB
  2. the frequency of bands that moved
  3. ±1.5 dB refinements
  4. a couple of repeated questions to check consistency; inconsistent answers are flagged

Your own songs need `ffmpeg` to decode them. Without it, the tuner offers synthesized loops
instead of the bundled excerpts.
