# Using neiro

Pick **Neiro DSP** as your output, if it isn't already the default, and play something.
Switch profiles and compare with processing switched off.

## Running it

```sh
neiro install     # systemd user service: starts now and at every login
neiro             # open the TUI
```

Or without the service:

```sh
neiro daemon      # leave this running; Ctrl+C stops it and restores your audio
neiro             # TUI, in another terminal
```

If the daemon isn't running, the TUI says so and can start it for you.

The badge at the top left shows whether neiro is processing your sound:
- **● ON**: sound plays through Neiro DSP.
- **BYPASS**: processing is switched off (`space`).
- **IDLE**: another device, such as headphones or a USB DAC, is your current output. The
  bottom line names it.
- **⚠ NOT IN USE**: the speaker itself is the default, so apps skip neiro.

Click **IDLE** or **NOT IN USE** to switch back to Neiro DSP.

## TUI

| Key | Action |
|---|---|
| `space` | Processing on / bypass |
| `1`–`9`, `0` | Profile, in tab order (dynamic, movie, music, game, voice, off, then any extras) |
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

**Meters:**
- **Per-band processing** (the bottom panel) has one column per frequency band:
  - The **bars** show how far each band is turned down to protect the speaker, by the
    multiband compressor and the regulator. As you turn up bass-heavy music, watch the bands
    your chassis resonates at get limited.
  - The **numbers** under the bars are the Intelligent EQ's current boost (cyan) or cut
    (magenta) in dB. They follow the music's average spectrum, so they change slowly.
- The **Levels** panel shows input and output loudness.

The **Personal EQ** stage has the [blind A/B tuner](tuner.md).

## Command line

```sh
neiro status                      # on/bypass, profile, latency
neiro toggle [on|off]             # flip or set processing (handy for a keyboard shortcut)
neiro profile music               # switch profile
neiro import <source> [--list]    # install this machine's tuning (see getting-your-tuning.md)
neiro dump-tuning [--mode M]      # print the tuning's modes, profiles and parameters
neiro render in.wav out.wav --profile music [--vbass] [--saved]   # process a 48 kHz WAV offline
```
