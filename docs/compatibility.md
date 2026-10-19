# Compatibility

neiro works on laptops that shipped with Dolby audio processing (Dolby Atmos / Dolby
Audio, "DAX3") on Windows, since those have a per-model speaker tuning to use.

## Tested on

- **ASUS Zenbook S14 (UX5406SA):** used day to day. This is where neiro was developed and
  listened to.
- **Parser and DSP:** checked against about 1,900 tuning files from public driver packages:
  - 262 from seven ASUS packages (Zenbook, Vivobook, TUF and ROG models);
  - 1,616 from four Lenovo packages (ThinkPad, IdeaPad, Yoga and all-in-one models,
    including convertibles with up to a dozen pose-specific tunings).

  Every operating mode and profile of every file loads and runs cleanly, over 27,000
  combinations in all. That shows they work, not that anyone has listened to them.

Reports from other laptops are welcome, whether it sounds right or not.

## Speakers

neiro processes two channels (left/right). That covers stereo laptops, including those whose
woofers and tweeters are split by the amplifiers' own firmware (the Zenbook S14 has four
speakers driven this way). A tuning that drives more speakers itself loads with a warning and
uses its front pair.

## Checking a batch of tunings

Developers can run any folder of tuning files through the parser and DSP:

```sh
NEIRO_CORPUS=/path/to/unpacked/driver cargo test --release corpus -- --ignored --nocapture
```

Never commit tuning files: they are the vendors' data.
