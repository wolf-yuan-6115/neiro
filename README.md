```
 __  __
/\ \/\ \          __
\ \ `\\ \     __ /\_\  _ __   ___
 \ \ , ` \  /'__`\/\ \/\`'__\/ __`\
  \ \ \`\ \/\  __/\ \ \ \ \//\ \L\ \
   \ \_\ \_\ \____\\ \_\ \_\\ \____/
    \/_/\/_/\/____/ \/_/\/_/ \/___/
```

# neiro (音色)

**Speaker enhancement for the ASUS Zenbook S14 on Linux.**

On Windows, the Zenbook S14's speakers go through the vendor's audio processing and sound
full and loud. On Linux they get none of it: the bass is thin, and the chassis buzzes in the
low mids when you turn it up. neiro brings that processing back. It is a PipeWire DSP node
that runs the laptop's own factory speaker tuning (taken from the Windows driver), with a
terminal UI to control it and a blind A/B test that tunes an EQ to your taste.

*neiro* is Japanese for tone colour, or timbre.

## Features

- **Factory tuning, per profile:** Dynamic, Movie, Music, Game, Voice and Off, read from
  the laptop's Windows tuning file.
- **Louder without the buzz:** a 20-band regulator holds down the chassis resonance
  (around 470–1030 Hz) while the rest of the spectrum, bass included, stays loud.
- **Full speaker chain:**
  - speaker correction EQ
  - adaptive "intelligent" EQ
  - multiband compressor
  - Volmax
  - lookahead limiter
  - optional psychoacoustic virtual bass
  - stereo widener
- **Personal EQ:** a 5-band EQ on top of every profile. Set it by hand, or let the
  **blind A/B tuner** find it with you.
- **TUI** with live meters, editing for every parameter, and full mouse support.
- **Plays nicely with the system:**
  - Volume keys control a "Neiro DSP" sink, and headphones are never touched.
  - Your default output, volume and mute are restored when neiro stops, even after a crash.
  - Shutdown fades out, so there's no pop.

## Requirements

- Linux with **PipeWire** and **WirePlumber**. neiro uses `pw-dump`, `wpctl`, `pw-metadata`
  and `pw-play`, which ship with them.
- To build: a **Rust** toolchain, the **PipeWire development headers** and **clang**, which
  the PipeWire bindings need. On Arch: `pipewire wireplumber rust clang`.
- **ffmpeg** (optional). The A/B tuner uses it to play the bundled samples and your own
  music. Without it, the tuner falls back to built-in synthesized loops.

neiro looks for the built-in speaker as an `alsa_output…Speaker` sink. The tuning is
specific to the Zenbook S14 it was taken from.

## Install

```sh
cargo install --path .      # puts `neiro` in ~/.cargo/bin
neiro install               # systemd user service: starts now and at every login
neiro                       # open the TUI
```

Or run it by hand without the service:

```sh
cargo build --release
./target/release/neiro daemon     # leave this running; Ctrl+C stops it and restores your audio
./target/release/neiro            # TUI, in another terminal
```

If the daemon isn't running, the TUI says so and can start it for you.

## Using it

Pick **Neiro DSP** as your output, if it isn't already the default, and play something.
Switch profiles and compare with processing switched off.

### TUI

| Key | Action |
|---|---|
| `space` | Processing on / bypass |
| `1`–`6` | Profile: dynamic, movie, music, game, voice, off |
| `Tab` | Switch focus between stages and parameters |
| `↑` `↓` (`k` `j`) | Select |
| `←` `→` (`h` `l`) | Adjust, with `Shift` for 10× steps |
| `Enter` / `e` | Toggle a stage or switch, or press a button |
| `s` | Save the current profile's changes |
| `R` `R` | Reset the profile to factory tuning |
| `q` | Quit the TUI (the daemon keeps running) |

**Mouse:**
- Click a tab, the ON/BYPASS badge, a stage, or a checkbox.
- Click or drag a slider, or use the wheel over it for fine steps.
- Hover anything for a hint. Hovering a band or level meter shows its exact values.

Hold `Shift` to select text in the terminal while the TUI is open.

The bottom panel shows how much each frequency band is being held back. Watch it shrink
the 470–1030 Hz bands as you turn up bass-heavy music. The **Levels** panel shows input
and output loudness.

### Finding your EQ with the blind A/B tuner

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

The test is **blind**: which side holds the change is random. Both sides are
**loudness-matched** (K-weighted, as in LUFS), so the louder one can't win just for being
louder. The search goes coarse to fine:

1. each band's gain ±3 dB
2. the frequency of bands that moved
3. ±1.5 dB refinements
4. a couple of repeated questions to check consistency

Pause other audio while tuning.

### Command line

```sh
neiro status                      # on/bypass, profile, latency
neiro toggle [on|off]             # flip or set processing (handy for a keyboard shortcut)
neiro profile music               # switch profile
neiro render in.wav out.wav --profile music [--vbass] [--saved]   # process a 48 kHz WAV offline
neiro dump-tuning                 # print the parsed factory profiles
```

## How it works

**Signal path.** `neiro daemon` creates the **Neiro DSP** sink. Apps play into it, neiro
processes the audio, and the result goes to the real speaker. Your volume is applied
*before* the processing, as on Windows. That way the speaker-protection stages see the
level that actually reaches the amplifier.

**Speaker volume.** While nothing else plays directly to the speaker, neiro holds the
hardware speaker at 100% and your volume keys work on Neiro DSP instead. If another app
uses the speaker directly, its own volume comes back immediately.

**Processing chain** (48 kHz, about 22 ms latency):

1. pre-gain → stereo widener
2. speaker PEQ: the factory filters, e.g. a −9 dB notch at 515 Hz for the chassis
3. virtual bass (optional): harmonics of the 35–160 Hz content that the small drivers
   can't reproduce
4. Personal EQ
5. a 20-band stage on the factory band layout:
   - intelligent EQ: slowly steers the spectrum toward Dolby's target curve
   - speaker correction EQ
   - Volmax
   - 4-band compressor
   - the **regulator**: a per-band soft-knee limiter using the factory thresholds
6. lookahead limiter → post-gain

Bypass crossfades to a delay-matched dry signal, so switching never clicks.

**Factory tuning.** The values come from `tuning.xml`, Dolby's DAX3 tuning shipped with the
laptop's Windows audio driver. A few of its fields aren't documented, so these readings are
inferred:
- the multiband compressor's field order;
- the regulator's 12 dB soft knee (`threshold_high − threshold_low`);
- how much limiting spreads to neighbouring bands (`timbre-preservation`).

All of them can be adjusted in the TUI.

## Files

| Path | What |
|---|---|
| `~/.config/neiro/state.toml` | Profile, bypass, saved per-profile changes, Personal EQ |
| `$XDG_RUNTIME_DIR/neiro.sock` | Control socket used by the TUI and CLI |
| `$XDG_RUNTIME_DIR/neiro.restore.json` | What to restore if the daemon crashes |

`state.toml` has two optional settings:
- `hw_volume`: holds the speaker below 100% (a value from 0 to 1).
- `tuning_path`: loads a different `tuning.xml`. `neiro daemon --tuning <file>` does the
  same for one run.

## Troubleshooting

- **Default output, volume or mute left wrong after a crash:** start the daemon again. It
  restores what the crashed run changed before taking over.
- **No sound through Neiro DSP:** check that its volume isn't muted or very low in your
  mixer. Then check `neiro status`, and whether `wpctl status` shows `output.neiro`
  connected to the speaker.

## Credits

The bundled tuner samples are short excerpts of Creative Commons tracks:
- **Night Owl** by Broke For Free (CC BY 3.0)
- **Like Tetanus In A Wound** by Hussalonia (CC BY 4.0)
- **Aces High** by Kevin MacLeod (CC BY 3.0)

Full credits, sources and the changes made are in [`samples/CREDITS.md`](samples/CREDITS.md).

The factory tuning file belongs to the laptop's vendors and is included for personal use.
neiro is not affiliated with or endorsed by ASUS or Dolby.
