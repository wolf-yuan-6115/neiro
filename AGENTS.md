# AGENTS.md

Guidance for AI coding agents working in this repository.

## What this is

neiro (音色) is a Rust speaker-enhancement DSP for the ASUS Zenbook S14 on PipeWire. It is
a single binary: a daemon that runs the audio node, and a ratatui TUI and CLI that control it.
The DSP reproduces the laptop's Windows factory tuning (`tuning.xml`, a Dolby DAX3 file
embedded with `include_str!`). It adds a personal EQ and a blind A/B tuner on top.

## Commands

```sh
cargo build --release                 # binary: target/release/neiro
cargo test --release                  # all tests (release: the DSP tests are slow in debug)
cargo test --release <name>           # one test or module, e.g. `regulator`, `tui::tests`
cargo test --release <name> -- --nocapture   # see printed renders/measurements

./target/release/neiro daemon         # run the PipeWire node + control socket (Ctrl+C restores audio)
./target/release/neiro                # TUI (default subcommand)
./target/release/neiro status | toggle [on|off] | profile <name>
./target/release/neiro render in.wav out.wav --profile music [--vbass] [--saved] [--stats]   # offline, 48 kHz only
./target/release/neiro dump-tuning    # parsed profiles as Params
./target/release/neiro segment <file> # hidden: which 10 s the tuner would pick
./target/release/neiro install        # writes and enables ~/.config/systemd/user/neiro.service
```

Some tests need `ffmpeg` on PATH, because the bundled samples and the clip tests decode Opus.
TUI tests render into ratatui's `TestBackend`. They record requests in `App::sent` instead of
talking to a daemon.

## Architecture

**Process split.** `neiro daemon` owns all state and audio. The TUI and CLI never touch audio.
They speak newline-delimited JSON over `$XDG_RUNTIME_DIR/neiro.sock` (`ipc.rs`: `Request` →
`Response { state, meters }`). `StateView` carries everything the TUI renders, including the
parameter descriptors.

**Parameter pipeline** (the data flow across modules):
1. `tuning.rs` parses `tuning.xml` into `Profile`s. Most values are 1/16 dB fixed point, converted to dB here.
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
  - The STFT stage applies optimizer, volmax, adaptive IEQ, MB compressor and regulator as per-bin gains.
- Latency is fixed: `chain::LATENCY` = 1024 (STFT) + 48 (limiter). Bypass crossfades to a dry path delayed by exactly that, bit-exact.
- Personal EQ runs two chains (A/B sides) that crossfade. This is what makes tuner switching click-free.
- Interpretation guesses for the undocumented Dolby fields are in code comments and the README: MB compressor field order, regulator knee = `threshold_high - threshold_low`, timbre preservation.
- IEQ removes the spectral tilt from both content and target before comparing (`bands::ieq_correction`).

**Daemon system integration** (`daemon.rs`, `system.rs`) shells out to `pw-dump`, `wpctl` and `pw-metadata`:
- At start, if the speaker (or neiro) is the default sink, it makes `neiro_sink` the default and copies the speaker's volume and mute onto it.
- A 1 s monitor loop pins the hardware speaker to 100% **only while nothing but `output.neiro` feeds it**. It unpins immediately otherwise.
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

## Live-testing cautions

Running the daemon changes the user's default sink and speaker volume. Use Ctrl+C or SIGTERM (by
PID) to stop it so it restores them, and confirm the speaker volume afterwards.
- Don't use `pkill -f '... neiro daemon'` from a shell whose own command line contains that
  pattern: it kills the shell.
- For TUI-only experiments, set `XDG_RUNTIME_DIR` to an empty directory so it can't reach a
  real daemon.
- `tuning.xml` is ASUS/Dolby material. It's fine in a private repo, but don't publish it.
  `--tuning` / `tuning_path` can load it at runtime instead.

## Conventions

Commit subjects use Conventional Commits (`feat(scope): …`, `fix: …`, `docs: …`). Do not add AI co-author or attribution trailers.
