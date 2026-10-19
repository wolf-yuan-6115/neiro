# Troubleshooting

## Logs

The daemon logs at `info` level by default. As a service, its log is in the journal:

```sh
journalctl --user -u neiro            # everything
journalctl --user -u neiro -p warning # warnings and errors only
```

For more detail, raise the level with `--log-level`, or `NEIRO_LOG` (which also takes
per-module filters such as `info,neiro::system=trace`):

```sh
neiro daemon --log-level debug   # requests, routing decisions, stream states
neiro daemon --log-level trace   # also every PipeWire tool call and TUI poll
```

For the service, run `systemctl --user edit neiro` and add `Environment=NEIRO_LOG=debug`
under `[Service]`. The TUI never logs, because it draws on the terminal.

## Common problems

**"no speaker tuning"**
Run `neiro import` first. See [Getting your tuning](getting-your-tuning.md).

**"none of the N tunings matches this machine"**
- The package may be for other models. Try your exact model's Dolby driver download.
- If you know which file is right, import it directly: `neiro import <file.xml>`.

**"couldn't tell which sink is the built-in speaker"**
Set `speaker_sink` in `~/.config/neiro/state.toml` to one of the names it lists.

**Default output, volume or mute left wrong after a crash**
Start the daemon again. It restores what the crashed run changed before taking over.

**"neiro is not in use" in the TUI**
Apps are playing straight to the speaker. Press `d` to make Neiro DSP the default; your
other devices keep their priority. `Esc` hides the warning until it happens again.

**No sound through Neiro DSP**
- Check that its volume isn't muted or very low in your mixer.
- Run `neiro status`.
- Check that `wpctl status` shows `output.neiro` connected to the speaker.

**Can't select text in the terminal while the TUI is open**
The TUI captures the mouse. Hold `Shift` while selecting.
