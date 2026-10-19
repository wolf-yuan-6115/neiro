# How it works

## Signal path

`neiro daemon` creates the **Neiro DSP** sink. Apps play into it, neiro processes the
audio, and the result goes to the real speaker. Headphones are a separate sink and are
never processed.

Your volume is applied *before* the processing, as on Windows. That way the
speaker-protection stages see the level that actually reaches the amplifier.

## Speaker volume

While nothing else plays directly to the speaker, neiro holds the hardware speaker at 100%
and your volume keys work on Neiro DSP instead. If another app uses the speaker directly,
its own volume comes back immediately. The limiting thresholds always account for the real
speaker volume.

When neiro stops, it fades out and gives the speaker back your volume and mute before
switching your apps over, so there's no pop and no burst at full volume. After a crash,
the next start restores everything first.

## Processing chain

48 kHz, about 22 ms latency:

1. pre-gain → stereo widener
2. **speaker PEQ:** the factory filters for each speaker (peaks, notches, shelves, high-
   and low-pass), typically aimed at the chassis resonances
3. **virtual bass** (optional): harmonics of the 35–160 Hz content that small drivers can't
   reproduce
4. **Personal EQ**
5. a 20-band stage on the factory band layout:
   - **intelligent EQ:** slowly steers the spectrum toward Dolby's target curve
   - speaker correction EQ
   - **Volmax**
   - multiband compressor (up to 4 groups)
   - the **regulator:** a per-band soft-knee limiter using the factory thresholds. This is
     what keeps the speaker loud without the chassis buzzing.
6. lookahead limiter → post-gain

Bypass crossfades to a delay-matched dry signal, so switching never clicks.

## Reading the factory tuning

Values come from your laptop's DAX3 tuning. Dolby doesn't document the format, and a few
fields are inferred:
- the multiband compressor's field order;
- the regulator's soft knee (`threshold_high − threshold_low`, usually 12 dB);
- how much limiting spreads to neighbouring bands (`timbre-preservation`);
- some speaker-filter types: 2 as a notch, 3 and 4 as high- and low-shelves, 6 and 8 as
  low-pass. Types 1 (peak) and 7/9 (high-pass) are unambiguous.

All of them can be adjusted in the TUI.
