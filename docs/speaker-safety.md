# Speaker safety

Laptop speakers are small and run close to their limits. neiro makes them louder and fuller,
so it also asks more of them. This page explains what that means over months of use and how
to keep the risk low.

neiro comes with no warranty (see the [license](../LICENSE)). You use it at your own risk.

## What can wear a speaker out

Damage rarely happens all at once. It builds up:
- **Heat:** loud playback for a long time heats the speaker's voice coil. Over time this
  weakens the glue and the coil, and the speaker can end up quieter, distorted or dead.
- **Movement:** strong bass makes the cone move further than it was designed to. That wears
  the suspension, which then rattles or buzzes.
- **Clipping:** a signal driven past full scale has harsh harmonics that the tweeter and the
  amplifier have to absorb.

The amplifier can be stressed in the same way: it runs hotter at high output for long periods.

## What neiro does about it

- **The per-band regulator** limits each frequency band at the thresholds in your laptop's
  factory tuning. That stops the chassis buzzing, and it holds the bass to a level the speaker
  can handle.
- **The lookahead limiter** keeps peaks below the tuning's ceiling, so the output never clips.
- **The thresholds follow the real speaker volume.** neiro holds the hardware speaker at 100%,
  so your volume keys turn down the audio before the processing instead (see
  [How it works](how-it-works.md#speaker-volume)).

With the factory settings, the result should be about as loud as the same laptop on Windows.

## Limits of that protection

- **The tuning is read, not documented.** Dolby doesn't publish the format, and a few fields,
  including the regulator's knee, are inferred (see
  [How it works](how-it-works.md#reading-the-factory-tuning)). If one is read wrongly, the
  protection can be weaker than on Windows.
- **Only one laptop has been listened to.** About 1,900 tunings load and run cleanly, but only
  the Zenbook S14's has been checked by ear for buzzing and distortion (see
  [Compatibility](compatibility.md)).
- **The amplifier's own protection is outside neiro.** Many laptops use "smart" amplifiers
  whose firmware watches the speaker's temperature and movement. On Windows the driver sets
  them up. On Linux that depends on your kernel and firmware files, and neiro can't see or
  change it. To see what your kernel loaded:

  ```sh
  journalctl -k -b | grep -i -E 'cs35l|tas2[57]|amplifier|firmware'
  ```

- **Every setting can be changed.** The TUI lets you go well past the factory values.

## Settings that remove protection

These are safe at their factory values. Raise or disable them only if you know what you're
doing, and try the change at low volume first:

| Setting | Risk |
|---|---|
| **Regulator** off, **Overdrive** up, or band thresholds up | Lets through the bass and resonances the factory tuning holds back. |
| **Post-gain** up | It comes after the regulator, so only the peak limiter stands behind it. |
| **Limiter** off | The output can clip. |
| **Volmax**, **Virtual bass** or **Bass loudness** gains up, or a big Personal EQ bass boost | More energy at the frequencies the speaker handles worst. The regulator still applies. |

The **Off** profile and **bypass** (`space`) skip the regulator and the limiter. They protect
the speaker no more than Linux without neiro does, so keep the volume moderate there.

## Good habits

- Keep the factory regulator and limiter on.
- If you hear buzzing, rattling or harsh distortion, turn down the volume first, then go back
  to the profile's factory settings (`R` `R` in the TUI).
- Avoid hours of bass-heavy playback near full volume, especially when the laptop is already
  hot, such as under heavy load or on a soft surface.
- Pay attention if the speakers sound different from before: quieter, dull, or buzzing at
  levels that used to be clean. Stop using neiro and compare with it stopped
  (`systemctl --user stop neiro`).
- To limit the maximum level, set `hw_volume` in `state.toml` to hold the speaker below 100%
  (see [Configuration](configuration.md)). The regulator thresholds follow it.
- For loud listening over long periods, use headphones or external speakers. neiro never
  processes them.

## Reporting a problem

If neiro makes your speakers buzz or distort at the factory settings, please report it with
your laptop model, the profile, and the volume where it starts. It may mean a field of your
tuning is being read wrongly. Don't attach the tuning file itself: it belongs to the vendor.
