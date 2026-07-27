# asciinematic

`asciinematic` is a command-aware terminal recorder with a tmux-like live control
menu. It records the complete PTY byte stream and indexes submitted commands in a
self-contained SQLite session.

There is no command-line interface. Running the program immediately starts your
normal `$SHELL` inside the recorder:

```sh
cargo build --release
target/release/asciinematic
```

Exit the shell normally to finish recording.

## Live controls

Press `Ctrl-T` twice while recording. The double-key prefix is consumed by
asciinematic and opens its alternate-screen menu. If another key follows the first
`Ctrl-T`, both keys are forwarded to the shell normally.

The current-history screen provides:

- current commands and a readable output preview;
- `Space` to anchor a range and `↑`/`↓` (or `j`/`k`) to extend it;
- `a` to select all commands and `x` to clear the range;
- `w` to checkpoint the active session;
- `s` to save the selected `k..k+n` range as a separate session;
- `r` to replay the command under the cursor;
- `e` to export the selected range as readable text;
- `b` to browse all saved sessions;
- `q` or `Esc` to return to the live terminal.

The saved-session browser provides:

- `↑`/`↓` to choose a session;
- `←`/`→` to choose one of its commands;
- an inspection pane containing that command's input and output;
- `r` to replay the selected command;
- `R` to replay the complete session;
- `e` to export the selected command as text;
- `q` or `Esc` to return to current history.

PTY output produced while either menu is visible remains timestamped and stored.
It is buffered from the display and flushed when you return to the terminal.

## Configuration

Configuration is environment-only:

- `SHELL` selects the child shell. It defaults to `/bin/sh`.
- `ASCIINEMATIC_NAME` sets the session name.
- `ASCIINEMATIC_HOME` overrides the storage directory.

On Linux, the default storage location is
`~/.config/asciinematic/<session-id>.sqlite3`.

## Storage

Every session database contains:

- `events`: timestamped input and output chunks stored as BLOBs;
- `commands`: exact submitted keystrokes and timeline boundaries;
- `metadata`: session id, name, program, start time, and duration.

Saved ranges are complete session databases, so they can be inspected and replayed
from the same interactive browser. Text exports are written beside the databases.

Command boundaries are inferred from Enter presses. Shell multiline continuations
and full-screen programs may not map one-to-one with logical commands, but the
underlying PTY event timeline remains byte-accurate.
