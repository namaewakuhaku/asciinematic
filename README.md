# asciinematic

`asciinematic` is a command-aware terminal recorder with a tmux-like live control
menu. It records the complete PTY output timeline and indexes confirmed shell
commands in a self-contained SQLite session.

Running the program without arguments immediately starts your normal `$SHELL`
inside the recorder:

```sh
cargo build --release
target/release/ascn
```

Exit the shell normally to finish recording.

Open the saved-session menu directly without starting a recording:

```sh
target/release/ascn sessions
```

Prebuilt Linux, Windows, and macOS archives are available from the repository's
GitHub Releases page. Maintainers publish a release by pushing a version tag:

```sh
git tag -a v0.1.0 -m "asciinematic v0.1.0"
git push build v0.1.0
```

## Live controls

Press `Ctrl-T` twice while recording. The double-key prefix is consumed by
asciinematic and opens its alternate-screen menu. If another key follows the first
`Ctrl-T`, both keys are forwarded to the shell normally.

The current-history screen provides:

- current commands and a rendered terminal snapshot;
- a bottom summary pane (generated asynchronously after recording);
- `Space` to anchor a range and `↑`/`↓` (or `j`/`k`) to extend it;
- `a` to select all commands and `x` to clear the range;
- `w` to checkpoint the active session;
- `s` to save the selected `k..k+n` range as a separate session;
- `r` to replay the command under the cursor;
- `e` to export the selected range as snapshot-based readable text and copy its path;
- `b` to browse all saved sessions;
- `q` or `Esc` to return to the live terminal.

The saved-session browser provides:

- `↑`/`↓` to choose a session;
- a left pane containing only sessions;
- a right pane containing the selected session's complete readable transcript;
- a bottom pane containing the selected session's 1–5 line summary;
- `PgUp`/`PgDn` to scroll the transcript;
- `Enter` to inspect command history with the live panel's range actions;
- `r` to replay the complete selected session;
- `e` to export its complete transcript and copy the path;
- `n` to rename the selected session interactively;
- `N` to leave the browser, start a new recording, and return when it finishes;
- `d`, then `d` again, to permanently delete the selected saved session;
- `c` to switch back to current-session controls when opened during recording;
- `q` or `Esc` to return or quit.

Mouse input is enabled while either menu is open. Use the wheel over a left-hand
list to change its selected command or session, click an item to select it, and
use the wheel over a right-hand preview or transcript to scroll its contents.

During any replay, use `Space` to pause or resume, `←`/`h` to jump back five
seconds, `→`/`l`/`f` to jump forward five seconds, `Home`/`End` to seek to the
beginning or end, and `q` or `Esc` to stop immediately.

PTY output produced while either menu is visible remains timestamped and stored.
It is buffered from the display and flushed when you return to the terminal.

## Configuration

Configuration is environment-only:

- `SHELL` selects the child shell. It defaults to `/bin/sh`.
- `ASCIINEMATIC_HOME` overrides the storage directory.

On Linux, the default storage location is
`~/.config/asciinematic/<session-uuid>`.

Session filenames and IDs are complete random UUIDs. New sessions display as
`Untitled` until the background agent generates a short title. The browser shows
the full UUID and a readable UTC timestamp beneath each title. Display names can
also be changed manually without renaming or destabilizing the database file. When
the browser is opened from a live recording, a blinking green dot follows that
session's UUID.

Session files intentionally have no extension. They identify themselves through
SQLite's file-format pragmas: `application_id = 0x41534349` (`ASCI`) and
`user_version = 2`. Existing version-1, `.sqlite`, and `.sqlite3` sessions remain
discoverable.

## Automatic titles and summaries

No model or inference runtime is embedded. When recording ends, asciinematic
detaches a small background copy of itself and immediately returns control to the
user. The worker looks for authenticated `codex` and then `claude` executables on
`PATH`.

Codex runs with `exec --ephemeral`; Claude runs with
`--print --no-session-persistence`. Both receive the transcript through standard
input in a one-shot task with tools disabled or read-only isolation. Their output
is reduced to a short human-readable title and 1–5 plain summary lines, then
stored in the session database. A manual rename is never overwritten by a late
background result.

Every submitted command and its rendered output, including unfinished final
output, is supplied. If neither agent is installed or authenticated, or both
fail, the session remains usable as `Untitled` and the bottom pane displays
`No summary available for this session.`

Saving a selected command range creates another UUID-named session and generates
an independent asynchronous summary for that range.

## Storage

Every session database contains:

- `events`: timestamped PTY output and confirmed command input stored as BLOBs;
- `commands`: exact submitted keystrokes and timeline boundaries;
- `metadata`: session UUID, title, summary, program, start time, and duration.

Saved ranges are complete session databases, so they can be inspected and replayed
from the same interactive browser. Text exports are written beside the databases,
and their canonical absolute paths are copied through the terminal's OSC 52
clipboard integration.

Sessions use SQLite's `DELETE` rollback-journal mode rather than WAL. The journal
exists only during an active transaction and is removed by SQLite afterward, so a
completed recording consists of its single extensionless database file. Finishing
an older WAL recording converts it and removes residual `-wal`/`-shm` sidecars.

Input is forwarded to the PTY immediately. A submitted line is stored as soon as
its raw or rendered shell echo confirms it, before result output or command
completion. This includes shell built-ins such as `cd`, `export`, and `alias`, even
when they produce no separate output. Shell redraws and line-editor cursor controls
are interpreted during confirmation;
unechoed password input and unrelated full-screen application interaction are
neither indexed nor persisted. Confirmed commands retain their exact raw
keystrokes.

Every PTY output chunk is committed independently with SQLite
`synchronous=FULL`, so completed commands are not required to retain partial
output. A crash, killed child, or interrupted final command still leaves all
chunks committed up to that point. Recordings containing no command other than
`exit` or `logout` are deleted instead of appearing as empty sessions.

Previews, exports, and summary context are rendered through a bounded 120×40 virtual
terminal with 256 lines of scrollback. Cursor movement, erasure, progress updates, and
full-screen redraws are applied before text is shown or written, so interactive programs
look like terminal snapshots instead of a flood of intermediate frames. Replay still uses
the original byte-accurate event timeline. Text exports place a visible separator between
command steps.

Command boundaries are inferred from raw or rendered shell echoes around Enter
submissions. Unusual shells which never display entered commands may not produce
command items, but their output history is still recorded.

## Release builds

The GitHub Actions release workflow runs when `main` or a tag is pushed, and can
also be started manually. It tests the project and publishes
checksum-protected workflow artifacts for:

- Linux x86-64 (musl);
- Linux ARM64 (musl);
- Windows x86-64;
- macOS universal (Intel and Apple Silicon).

Linux and Windows are compiled with `cross`. Because its distributed images
cannot include Apple's SDK, the macOS artifact is compiled with the official
`cargo-zigbuild` container, which includes the SDK and supports universal
binaries. All four archives and `SHA256SUMS` appear on the workflow run.
