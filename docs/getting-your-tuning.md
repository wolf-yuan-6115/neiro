# Getting your tuning

neiro runs your laptop's own speaker tuning: the Dolby DAX3 XML that ships inside its
Windows audio driver. It's the laptop vendor's data, so neiro doesn't include one.

## Import

`neiro import` finds the file that belongs to your machine and installs it to
`~/.config/neiro/tuning.xml`:

```sh
neiro import /mnt/windows                 # a mounted Windows partition (searches its DriverStore)
neiro import ~/Downloads/driver-unpacked  # an unpacked driver package
neiro import DEV_0287_SUBSYS_….xml        # or the file itself
neiro import <source> --list              # only show what matches
```

It reads your audio hardware's IDs (SoundWire devices, HD-Audio codecs and the audio
controller) and matches them against the tuning file names. An exact match wins. A file
that only shares the subsystem id is used with a warning, because it may be for a sibling
model. If several files match, the newest tuning version wins.

Restart the daemon after importing.

## No Windows partition?

Download your model's audio driver from the vendor's support site and unpack it. Not every
audio download contains the tuning; you want the Dolby one.

| Vendor | Download | Unpack with |
|---|---|---|
| ASUS | *Dolby Atmos driver* (Software and Utility, or Audio) | `7z x -tPE -opkg FILE.exe .rsrc/ZIP/103`, then `7z x -opkg/tree pkg/.rsrc/ZIP/103` |
| Lenovo | the audio driver `.exe` | `innoextract -d pkg FILE.exe` |
| Framework | the driver bundle `.exe` | `7z x -opkg FILE.exe` |
| Microsoft Surface | the drivers and firmware `.msi` | `msiextract -C pkg FILE.msi` |
| Samsung | the audio driver `.zip` | `unzip -d pkg FILE.zip` |

Then run `neiro import pkg`.

The [speaker-tuning-to-easyeffects](https://github.com/antoinecellerier/speaker-tuning-to-easyeffects)
project, whose research into these files neiro builds on, covers this in more detail. It
also keeps a list of laptops confirmed to work with their tool.

## Convertibles

Some tunings have one variant per pose (tablet, tent, stand, …). neiro uses `normal` by
default. `neiro dump-tuning` lists the modes your tuning has. To use another one, set it in
`~/.config/neiro/state.toml`:

```toml
mode = "tablet"
```
