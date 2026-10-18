```
 __  __
/\ \/\ \          __
\ \ `\\ \     __ /\_\  _ __   ___
 \ \ , ` \  /'__`\/\ \/\`'__\/ __`\
  \ \ \`\ \/\  __/\ \ \ \ \//\ \L\ \
   \ \_\ \_\ \____\\ \_\ \_\\ \____/
    \/_/\/_/\/____/ \/_/\/_/ \/___/
```

# neiro (音色) — speaker enhancement for the Zenbook S14 on Linux

*neiro* (音色) is Japanese for tone colour, or timbre.

A PipeWire DSP node and TUI that reproduce the Dolby DAX3 speaker processing
from the laptop's Windows factory tuning (`tuning.xml`, embedded at build time).

```
cargo install --path .     # puts `neiro` in ~/.cargo/bin
neiro install                # systemd user service, starts now and at login
neiro                        # TUI
```

Other commands: `neiro toggle [on|off]`, `neiro profile music`, `neiro status`,
`neiro render in.wav out.wav --profile music [--vbass]` (48 kHz offline A/B),
`neiro dump-tuning`.

## Personal EQ and the blind A/B tuner

On top of Dolby's per-profile tuning, a global 5-band **Personal EQ** applies to every profile: bass shelf, low-mid, mid, presence, treble shelf. You can set it by hand in the TUI, or let neiro find it. In the TUI, open **Personal EQ**, select **▶ Find my EQ: blind A/B test** at the top and press Enter, then choose music: one of three bundled Creative Commons excerpts (Night Owl by Broke For Free, Like Tetanus In A Wound by Hussalonia, Aces High by Kevin MacLeod; credits in `samples/CREDITS.md`), or any song under `~/Music`, where neiro plays the song's fullest 10 seconds rather than the intro. Each trial compares your current EQ with one small change:

- `a` / `b` (or `space`) switch what you hear
- `1` / `2` pick the better side, `0` if you can't tell
- `x` switches between A and B every 4 s

The test is blind: which side holds the change is random, and both sides are loudness-matched with LUFS-style K-weighting, so "louder" can't win. The search goes coarse to fine:

1. each band's gain ±3 dB
2. the frequency of bands that moved
3. ±1.5 dB refinement
4. two consistency re-checks

That's about 20–30 answers. At the end you see the curve and choose to keep or discard it. Pause other audio while tuning.

## How it works

`neiro daemon` creates a **Neiro DSP** sink (`neiro_sink`) and plays the
processed signal into the hardware speaker sink. Your volume keys control the
virtual sink, so the DSP sees post-volume audio, like Dolby's endpoint APO does.
The hardware speaker is pinned at 100% only while nothing but neiro feeds it. If
anything else plays directly to the speaker, its old volume and mute come back
at once. The regulator thresholds always compensate for the real hardware volume.
On exit (or at the next start after a crash) the previous default sink comes back,
and the speaker takes over the volume and mute you last set on the virtual sink. Headphones are a separate sink and are never processed.

Chain (48 kHz, ~22 ms latency):

1. pre-gain → stereo widener (side boost > 300 Hz)
2. speaker PEQ (factory biquads, e.g. the 515 Hz −9 dB Q9 chassis notch in Music)
3. virtual bass (optional): Chebyshev 2nd–4th harmonics of 35–160 Hz content, using Dolby's sub-gains
4. 20-band STFT stage (Dolby's band centres):
   - intelligent EQ: steers the long-term spectrum toward the `ieq_*` target
   - audio optimizer: static 20-band speaker correction
   - Volmax boost
   - 4-group multiband compressor
   - **regulator**: a per-band soft-knee limiter using the factory thresholds. It keeps the 470–1030 Hz chassis resonance down while bass stays loud.
5. lookahead brickwall limiter → post-gain

Bypass is a 20 ms crossfade to a latency-matched dry signal, so toggling is click-free.

## Tuning-file interpretation

Values are in 1/16 dB. These parts are inferred, not documented by Dolby:

- MB compressor `band_group` = end band, threshold, Q15 slope (1/ratio), attack ms, release ms, makeup.
- Regulator `threshold_high` − `threshold_low` = 12 dB, treated as a soft-knee width.
- `timbre-preservation` (12/16) = the share of gain reduction spread to non-isolated neighbouring bands.

Every one of these is adjustable in the TUI.
Settings: `~/.config/neiro/state.toml`. Set `hw_volume` there to pin the amp below 100%.
