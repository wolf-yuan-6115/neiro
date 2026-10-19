# Configuration

## Files

| Path | What |
|---|---|
| `~/.config/neiro/tuning.xml` | Your laptop's tuning, installed by `neiro import` |
| `~/.config/neiro/state.toml` | Profile, bypass, saved per-profile changes, Personal EQ |
| `$XDG_RUNTIME_DIR/neiro.sock` | Control socket used by the TUI and CLI |
| `$XDG_RUNTIME_DIR/neiro.restore.json` | What to restore if the daemon crashes |

## Settings in `state.toml`

Most of `state.toml` is written by neiro itself. These optional settings are meant to be
set by hand:

| Setting | Effect |
|---|---|
| `mode` | The tuning's operating mode on convertibles, e.g. `"tablet"`. `neiro dump-tuning` lists them. |
| `speaker_sink` | PipeWire name of your built-in speaker, if neiro picks the wrong sink. By default it takes the ALSA sink whose name says "speaker". |
| `hw_volume` | Holds the speaker below 100% while neiro runs (0 to 1). |
| `tuning_path` | Uses a tuning from elsewhere. `neiro daemon --tuning <file>` does the same for one run. |

Restart the daemon after changing them.
