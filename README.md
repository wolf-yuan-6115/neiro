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

**Laptop speaker enhancement for Linux, using your laptop's own Dolby tuning.**

Many laptops sound full and loud on Windows thanks to Dolby processing tuned for that
model, then thin and buzzy on Linux. neiro brings the processing back. It runs your
laptop's factory tuning as a PipeWire filter, with a terminal UI to control it.

*neiro* is Japanese for tone colour, or timbre.

## Features

- Your laptop's factory tuning, with all its profiles (Dynamic, Movie, Music, …)
- Louder speakers without chassis buzz, using the factory per-band limits
- A personal EQ, set by hand or found with a blind A/B listening test
- A terminal UI with live meters, mouse support and every parameter adjustable
- Restores your output, volume and mute when it stops, even after a crash

## Requirements

- Linux with PipeWire and WirePlumber
- **A laptop that came with Dolby audio on Windows**, neiro does not generate tuning by itself.
- Rust, clang and the PipeWire headers to build from source
- ffmpeg (optional, for the A/B tuner)

> **Speaker safety:** like Dolby on Windows, neiro drives your speakers harder than plain
> Linux does. Its protection follows the factory tuning, but parts of it are inferred, and
> it can be turned off. Read [Speaker safety](docs/speaker-safety.md) before raising the limits.

## Getting started

**1. Install neiro**

On Arch Linux, install [`neiro-git`](https://aur.archlinux.org/packages/neiro-git) from
the AUR:

```sh
paru -S neiro-git   # or yay -S neiro-git
```

Elsewhere, build it from source:

```sh
cargo install --path .
```

**2. Import your laptop's tuning.** Point neiro at a mounted Windows partition, or at your
laptop's Dolby driver download after unpacking it. neiro picks the file that matches your
hardware.

```sh
neiro import /mnt/windows
```

No Windows partition? [Getting your tuning](docs/getting-your-tuning.md) shows where to
download the driver and how to unpack it.

**3. Start it.** This runs neiro as a user service, now and at every login:

```sh
neiro install
```

The AUR package already ships the service, so there use
`systemctl --user enable --now neiro.service` instead.

To try it without the service, run `neiro daemon` in a terminal. Ctrl+C stops it and
restores your audio.

**4. Listen.** neiro adds a **Neiro DSP** output and makes it the default. Play something
and open the TUI:

```sh
neiro
```

| Key | Action |
|---|---|
| `space` | Processing on / off, to compare |
| `1`–`9`, `0` | Switch profile |
| `↑` `↓`, `←` `→` | Pick a setting, adjust it |
| `s` | Save your changes |
| `q` | Quit (neiro keeps running) |

The mouse works too: click, drag sliders, and hover for hints.

**5. Make it yours (optional).** Under **Personal EQ**, **▶ Find my EQ** runs a blind A/B
listening test that tunes an EQ to your ears. See [the tuner](docs/tuner.md).

From the command line, `neiro toggle` switches processing on or off, `neiro profile music`
switches profile, and `neiro status` shows what's running.

## Documentation

- [Getting your tuning](docs/getting-your-tuning.md): import, vendor driver downloads, convertibles
- [Using neiro](docs/usage.md): the TUI, mouse, meters and command line
- [The blind A/B tuner](docs/tuner.md): finding a personal EQ by ear
- [Configuration](docs/configuration.md): files and settings
- [How it works](docs/how-it-works.md): signal path, volume handling, the processing chain
- [Compatibility](docs/compatibility.md): what's been tested, speakers, reporting your laptop
- [Speaker safety](docs/speaker-safety.md): long-term wear, what protects the speakers, settings to be careful with
- [Troubleshooting](docs/troubleshooting.md)

## Credits

- The research into reading Dolby tunings on Linux comes from
  [speaker-tuning-to-easyeffects](https://github.com/antoinecellerier/speaker-tuning-to-easyeffects)
  by Antoine Cellerier.
- The tuner's sample excerpts are Creative Commons tracks by Broke For Free, Hussalonia and
  Kevin MacLeod. See [`samples/CREDITS.md`](samples/CREDITS.md).

Tuning files belong to the laptop and audio vendors. neiro reads the one on your machine
and never redistributes them. neiro is not affiliated with or endorsed by Dolby or any
laptop maker.

## License

Copyright (C) 2026 Wolf Yuan <me@wolf-yuan.dev>

neiro is free software: you can redistribute it and/or modify it under the terms of the
GNU General Public License as published by the Free Software Foundation, either version 3
of the License, or (at your option) any later version. See [`LICENSE`](LICENSE).

The sample excerpts in `samples/` keep their own Creative Commons licenses, listed in
[`samples/CREDITS.md`](samples/CREDITS.md).
