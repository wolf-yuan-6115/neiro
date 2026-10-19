# Troubleshooting

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
