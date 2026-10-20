# AGENTS.md

Guidance for AI coding agents working in this repository.

## What this is

neiro (音色) is a Rust laptop-speaker DSP for PipeWire. It is a single binary: a daemon
that runs the audio node, and a ratatui TUI and CLI that control it. The DSP reproduces the
laptop's Windows factory tuning: the Dolby DAX3 XML from its audio driver, installed at
runtime to `~/.config/neiro/tuning.xml` by `neiro import`. It is never embedded or committed.
A personal EQ and a blind A/B tuner sit on top. It was developed on an ASUS Zenbook S14,
but nothing in the code is model-specific.

## Commands

```sh
cargo build --release                 # binary: target/release/neiro
cargo test --release                  # all tests (release: the DSP tests are slow in debug)
cargo test --release <name>           # one test or module, e.g. `regulator`, `tui::tests`
cargo test --release <name> -- --nocapture   # see printed renders/measurements

./target/release/neiro daemon         # run the PipeWire node + control socket (Ctrl+C restores audio)
./target/release/neiro                # TUI (default subcommand)
./target/release/neiro status | toggle [on|off] | profile <name>
./target/release/neiro render in.wav out.wav --profile music [--vbass] [--loudness] [--volume-db=-20] [--saved] [--stats]   # offline, 48 kHz only
./target/release/neiro import <xml|dir|windows-mount> [--list]   # match + install this machine's tuning
./target/release/neiro dump-tuning [--tuning F] [--mode M]   # modes, profiles, parsed Params
./target/release/neiro segment <file> # hidden: which 10 s the tuner would pick
./target/release/neiro install        # writes and enables ~/.config/systemd/user/neiro.service
```

Some tests need `ffmpeg` on PATH, because the bundled samples and the clip tests decode Opus.
Unit tests use `tests/fixtures/synthetic_dax3.xml` (`tuning::test_tuning()`), a hand-made
tuning with invented values that covers every schema variant neiro handles.

The opt-in corpus test runs real tunings through discovery, the parser and the DSP. Point
it at an unpacked driver package (`docs/getting-your-tuning.md` says how to unpack each
vendor's):
`NEIRO_CORPUS=/path cargo test --release corpus -- --ignored --nocapture`.
TUI tests render into ratatui's `TestBackend`. They record requests in `App::sent` instead of
talking to a daemon.

## Architecture

**Process split.** `neiro daemon` owns all state and audio. The TUI and CLI never touch audio.
They speak newline-delimited JSON over `$XDG_RUNTIME_DIR/neiro.sock` (`ipc.rs`: `Request` →
`Response { state, meters }`). `StateView` carries everything the TUI renders, including the
parameter descriptors.

**Parameter pipeline** (the data flow across modules):
1. `tuning.rs` parses a DAX3 tuning into `Profile`s for one `internal_speaker` operating mode.
   - Most values are 1/16 dB fixed point, converted to dB here.
   - Elements may be missing, use `value=`, `preset=` or per-channel `<ch_00>` forms, and speaker PEQ is per speaker (left/right). Everything falls back to neutral defaults.
   - `Tuning::profile_names()` puts the canonical profiles first and always includes `off`, synthesised by `params::factory` when the file has none.
2. `params.rs`: `Params::from_profile` gives the factory defaults per profile.
   - User edits are full per-profile overrides in `Saved` (`~/.config/neiro/state.toml`).
   - The global `PersonalEq` also lives in `Saved`.
   - `params::sections()` builds the TUI's descriptors. Each descriptor `path` is a JSON pointer into `Params`; a `/personal/...` prefix is routed to `Saved.personal` instead.
3. `dsp::chain::Config::build` turns `Params` into a `Copy`, allocation-free `Config`.
   - The daemon (`daemon.rs`, `Inner::push`) sends it to the audio thread through an `rtrb` queue.
   - The audio thread sends the old box back on a second queue, so it is freed off the RT thread.
4. Meters flow back through `SharedMeters` atomics. Peaks use `fetch_max` on f32 bits and are reset on read.

**Audio node** (`pw.rs`) follows PipeWire's module-loopback pattern:
- A capture stream with `media.class=Audio/Sink` (`neiro_sink`, shown as "Neiro DSP") has the `ASYNC` flag and only triggers the playback stream.
- The playback stream (`output.neiro`, `TRIGGER`) dequeues both buffers and runs `Engine::process`.
- Both streams share `node.group`/`node.link-group`, and set `state.restore-props=false` so WirePlumber doesn't restore stale volumes.
- The app's volume on the virtual sink is applied *before* the DSP. That matters because the regulator thresholds are absolute levels at the amp.

**DSP chain** (`dsp/chain.rs` `run_wet`):
- Order: pregain → widener → speaker PEQ → virtual bass → personal EQ → 20-band STFT stage (`dsp/bands.rs`) → postgain → lookahead limiter.
  - The STFT stage applies optimizer, volmax, adaptive IEQ, bass loudness, MB compressor and regulator as per-bin gains.
  - Bass loudness is not from the tuning file. It boosts the low bands by how far the level of the bands above 200 Hz is below a reference, so it tracks the sink volume. Old saved overrides lack `Params::loudness`, hence `#[serde(default)]`.
- Latency is fixed: `chain::LATENCY` = 1024 (STFT) + 48 (limiter). Bypass crossfades to a dry path delayed by exactly that, bit-exact.
- Personal EQ runs two chains (A/B sides) that crossfade. This is what makes tuner switching click-free.
- Interpretation guesses for the undocumented Dolby fields are in code comments and the README: MB compressor field order, regulator knee = `threshold_high - threshold_low`, timbre preservation.
- IEQ removes the spectral tilt from both content and target before comparing (`bands::ieq_correction`).

**Tuning discovery** (`discover.rs`, `neiro import`):
- `MachineIds::detect` reads SoundWire (manufacturer, part) from `/sys/bus/soundwire/devices`, the audio controller's PCI subsystem (classes `0x0401`/`0x0403`), and HD-Audio codecs from `/proc/asound/card*/codec#*`.
- `parse_name` decodes the DAX3 file-name conventions. It skips the `_settings`/`_dmic`/`_amic` companions.
- `rank` orders candidates: exact match, then subsystem-only (warned), then `tuning_version`.

The matching rules follow the speaker-tuning-to-easyeffects project's findings.

**Daemon system integration** (`daemon.rs`, `graph.rs`, `system.rs`):
- `graph.rs` mirrors the PipeWire graph from registry events on the main loop: nodes, links, the `default` metadata, and the Props volume of the speaker and `neiro_sink`. The monitor thread wakes on its changes; nothing polls.
- Writes (volume, mute, default sink) and the reads before the main loop starts or after it stops shell out to `pw-dump`, `wpctl` and `pw-metadata` (`system.rs`).
- The speaker sink is `speaker_sink` from `state.toml` if set. Otherwise `system::find_speaker` considers only built-in ALSA sinks (never USB, HDMI/DisplayPort, headphone or Bluetooth, even when one is the default). It takes the one whose name or description says "speaker", or the only one left.
- At start, if the speaker (or neiro) is the default sink, `become_default` makes `neiro_sink` the default and copies the speaker's volume and mute onto it:
  - If the configured default is another, absent device (a USB DAC, Bluetooth), neiro leaves it configured. It re-selects the sinks above the speaker in WirePlumber's history (`~/.local/state/wireplumber/default-nodes`), so `neiro_sink` lands just above the speaker. That device still wins when it returns (`Restore::stand_in`).
  - Otherwise it takes the configured slot and restores it on exit.
- `StateView::default_output` is the effective default sink. When it isn't `neiro_sink`, the TUI shows a modal notice: a warning when it's the speaker (apps skip neiro by mistake), an info notice for any other device. `Request::MakeDefault` is acted on by the monitor thread: `become_default` when the speaker is the default, otherwise neiro takes the configured slot, like choosing it in the sound menu.
- The monitor pins the hardware speaker to 100% **only while nothing but `output.neiro` feeds it**. It unpins immediately otherwise.
- The regulator thresholds are always offset by the real speaker volume (`hw_comp_db`).
- Restore info is mirrored to `$XDG_RUNTIME_DIR/neiro.restore.json` for crash recovery on the next start.
- Shutdown order is load-bearing (see `on_quit`):
  1. Set `stopping`; the RT thread fades the output out over 50 ms.
  2. Restore speaker volume and mute.
  3. Restore the default sink.
  4. Wait for streams to move, then disconnect.

  Reordering this brings back a pop or a burst at full volume.
- SIGINT/SIGTERM are blocked before any thread spawns, so PipeWire's signalfd sources receive them.

**TUI** (`tui.rs`):
- Each frame clears `App::hits` and records `(Rect, Hit)` for every clickable element. Mouse clicks, drags, the wheel and hover all resolve against that list (last entry wins).
- `RowBuilder` builds lines while recording button areas.
- Hover styling is decided at draw time from `App::mouse`.

**A/B tuner:**
- `tuner.rs` is a pure state machine: blind side assignment, coarse-to-fine search, consistency checks.
- `clip.rs` handles audio:
  - decodes with ffmpeg, either a file or embedded bytes through stdin;
  - picks the fullest 10 s of a song (`best_segment`);
  - level-matches the two sides with K-weighted loudness (`loudness_delta`);
  - writes a crossfaded loop to `$XDG_RUNTIME_DIR/neiro-clip.wav` and plays it into `neiro_sink` with `pw-play`.
- `samples.rs` embeds the CC-BY excerpts in `samples/`. A test fails unless each one is credited in `samples/CREDITS.md`.
- `demo.rs` has synthesized loops, used only when ffmpeg is missing.

**Logging** (`logging.rs`) uses `log` + `env_logger`. The level comes from `--log-level`, else `NEIRO_LOG`, else `info` for the daemon and `warn` for CLI commands. Under systemd, stderr is the journal (`JOURNAL_STREAM`), so lines get `<N>` syslog priority prefixes and no timestamps. Never initialise logging for the TUI; use `log::` macros, not `eprintln!`, in code the daemon runs. `system::run` logs every PipeWire tool call at trace level.

## Packaging and CI

`.gitlab-ci.yml` tests and builds on Debian 12 (`rust:1-bookworm`, glibc 2.36, PipeWire
0.3.65 headers). Every pipeline produces three artifacts:
- a binary tarball;
- a `.deb`, via `cargo deb`;
- an `.rpm`, via `cargo generate-rpm`.

Both package tools read their metadata from `Cargo.toml`. A `v<version>` tag that matches
`Cargo.toml` also uploads the artifacts to the package registry and creates a release.
Its notes come from `packaging/release-notes.sh`. It groups the commit subjects since the
previous tag: `feat`, `fix` and `perf` first, then every other type under "Other changes".
So commit subjects double as the changelog.
Preview them with `packaging/release-notes.sh <version> HEAD`. The
systemd user unit lives in `packaging/neiro.service`.

- **Old PipeWire headers:** the code must compile against PipeWire 0.3.65. Use numeric
  values for newer constants; see `PW_STREAM_FLAG_ASYNC` in `pw.rs`.
- **Package names:** neiro calls `pw-dump`/`pw-metadata`/`pw-play` and `wpctl` at runtime.
  Those come from `pipewire-bin` + `wireplumber` on Debian, `pipewire-utils` +
  `wireplumber` on Fedora, and `pipewire` + `pipewire-audio` + `wireplumber` on Arch.

## Live-testing cautions

Running the daemon changes the user's default sink and speaker volume. Use Ctrl+C or SIGTERM (by
PID) to stop it so it restores them, and confirm the speaker volume afterwards.
- Don't use `pkill -f '... neiro daemon'` from a shell whose own command line contains that
  pattern: it kills the shell.
- For TUI-only experiments, set `XDG_RUNTIME_DIR` to an empty directory so it can't reach a
  real daemon.
- Never commit vendor tuning XMLs or a corpus of them; they are third-party data. Only the
  synthetic fixture belongs in the repo.

## Conventions

Commit subjects use Conventional Commits (`feat(scope): …`, `fix: …`, `docs: …`). Do not add AI co-author or attribution trailers.
