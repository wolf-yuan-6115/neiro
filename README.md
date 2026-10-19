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
- A laptop that came with Dolby audio on Windows
- Rust, clang and the PipeWire headers to build
- ffmpeg (optional, for the A/B tuner)

## Quick start

```sh
cargo install --path .        # build and install `neiro`
neiro import /mnt/windows     # find and install your laptop's tuning
neiro install                 # run it as a user service, now and at login
neiro                         # open the TUI
```

No Windows partition? [Getting your tuning](docs/getting-your-tuning.md) explains how to
get the tuning from your vendor's driver download.

## Documentation

- [Getting your tuning](docs/getting-your-tuning.md): import, vendor driver downloads, convertibles
- [Using neiro](docs/usage.md): the TUI, mouse, meters and command line
- [The blind A/B tuner](docs/tuner.md): finding a personal EQ by ear
- [Configuration](docs/configuration.md): files and settings
- [How it works](docs/how-it-works.md): signal path, volume handling, the processing chain
- [Compatibility](docs/compatibility.md): what's been tested, speakers, reporting your laptop
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
