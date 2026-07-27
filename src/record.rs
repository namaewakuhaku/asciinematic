use std::{
    collections::VecDeque,
    env,
    io::{self, Read, Write},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Instant,
};

use anyhow::{Context, Result};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use crate::{
    store::{self, INPUT, OUTPUT},
    summary, text, tui,
};

#[derive(Default)]
struct DisplayGate {
    menu_active: bool,
    buffered_output: Vec<u8>,
}

struct PendingCommand {
    started_us: i64,
    submitted_us: i64,
    output_offset: usize,
    raw: Vec<u8>,
    needle: Vec<u8>,
    boundary_on_echo: bool,
}

struct ConfirmedCommand {
    ordinal: i64,
    input_us: i64,
    started_us: i64,
    raw: Vec<u8>,
}

#[derive(Default)]
struct CommandTracker {
    output: VecDeque<u8>,
    output_base: usize,
    output_seen: usize,
    pending: VecDeque<PendingCommand>,
    ordinal: i64,
}

impl CommandTracker {
    const OUTPUT_WINDOW: usize = 128 * 1024;
    const CONFIRMATION_WINDOW_US: i64 = 5_000_000;

    fn output_offset(&self) -> usize {
        self.output_seen
    }

    fn begin_line(&mut self, now_us: i64) -> Vec<ConfirmedCommand> {
        let confirmed = self.confirm(now_us);
        // Keep unconfirmed lines submitted in this same input batch. This is how pasted
        // multi-line commands arrive; older unechoed input is invalidated so password
        // responses cannot be matched by unrelated later output.
        self.pending
            .retain(|candidate| candidate.submitted_us == now_us);
        confirmed
    }

    fn submit(&mut self, mut command: PendingCommand) -> Vec<ConfirmedCommand> {
        // An unechoed older line is application input (commonly a password), not a shell
        // command. Never allow later terminal output to make it match retroactively.
        let mut confirmed = self.confirm(command.submitted_us);
        self.pending
            .retain(|candidate| candidate.submitted_us == command.submitted_us);
        command.boundary_on_echo = self
            .pending
            .iter()
            .any(|candidate| candidate.submitted_us == command.submitted_us);
        self.pending.push_back(command);
        confirmed.extend(self.confirm(self.pending.back().map_or(0, |item| item.submitted_us)));
        confirmed
    }

    fn observe_output(&mut self, bytes: &[u8], now_us: i64) -> Vec<ConfirmedCommand> {
        self.output.extend(bytes);
        self.output_seen = self.output_seen.saturating_add(bytes.len());
        while self.output.len() > Self::OUTPUT_WINDOW {
            self.output.pop_front();
            self.output_base = self.output_base.saturating_add(1);
        }
        self.confirm(now_us)
    }

    fn confirm(&mut self, now_us: i64) -> Vec<ConfirmedCommand> {
        let output = self.output.make_contiguous();
        let output_base = self.output_base;
        let mut confirmed = Vec::new();
        self.pending.retain(|candidate| {
            let start = candidate
                .output_offset
                .saturating_sub(output_base)
                .min(output.len());
            // Match against the rendered terminal state. Shell line editors redraw input
            // with cursor controls, so looking for contiguous raw bytes can miss a command
            // that was plainly visible before Enter was pressed.
            let rendered = text::terminal_snapshot(&output[start..]);
            let echoed = contains_bytes(rendered.as_bytes(), &candidate.needle);
            let expired =
                now_us.saturating_sub(candidate.submitted_us) > Self::CONFIRMATION_WINDOW_US;
            if echoed {
                self.ordinal += 1;
                confirmed.push(ConfirmedCommand {
                    ordinal: self.ordinal,
                    input_us: candidate.started_us,
                    started_us: if candidate.boundary_on_echo {
                        now_us
                    } else {
                        candidate.started_us
                    },
                    raw: candidate.raw.clone(),
                });
            }
            !echoed && !expired
        });
        confirmed
    }

    fn split_points(&self, bytes: &[u8]) -> Vec<usize> {
        let mut points = self
            .pending
            .iter()
            .filter_map(|candidate| {
                bytes
                    .windows(candidate.needle.len())
                    .position(|window| window == candidate.needle)
            })
            .filter(|position| *position > 0)
            .collect::<Vec<_>>();
        points.sort_unstable();
        points.dedup();
        points
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn persist_confirmed(conn: &rusqlite::Connection, commands: Vec<ConfirmedCommand>) -> Result<()> {
    for command in commands {
        store::append_event(conn, command.input_us, INPUT, &command.raw)?;
        store::add_command(conn, command.ordinal, command.started_us, &command.raw)?;
    }
    Ok(())
}

struct RawModeGuard(bool);

impl RawModeGuard {
    fn enable_if_terminal() -> Result<Self> {
        if std::io::IsTerminal::is_terminal(&io::stdin()) {
            enable_raw_mode()?;
            Ok(Self(true))
        } else {
            Ok(Self(false))
        }
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.0 {
            let _ = disable_raw_mode();
        }
    }
}

fn elapsed_us(start: Instant) -> i64 {
    start.elapsed().as_micros().min(i64::MAX as u128) as i64
}

pub fn run(path: &Path, program: &Path) -> Result<i32> {
    let _raw_mode = RawModeGuard::enable_if_terminal()?;
    let (cols, rows) = size().unwrap_or((80, 24));
    let pair = native_pty_system().openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut command = CommandBuilder::new(program);
    command.env(
        "TERM",
        env::var("TERM").unwrap_or_else(|_| "xterm-256color".into()),
    );
    let mut child = pair
        .slave
        .spawn_command(command)
        .with_context(|| format!("failed to launch {}", program.display()))?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    let start = Instant::now();
    let done = Arc::new(AtomicBool::new(false));
    let display_gate = Arc::new(Mutex::new(DisplayGate::default()));
    let command_tracker = Arc::new(Mutex::new(CommandTracker::default()));
    let output_path = path.to_owned();
    let output_done = Arc::clone(&done);
    let output_gate = Arc::clone(&display_gate);
    let output_tracker = Arc::clone(&command_tracker);
    let output_thread = thread::spawn(move || -> Result<()> {
        let conn = store::open_event_writer(&output_path)?;
        let mut buffer = [0_u8; 8192];
        let mut last_event_us: i64 = -1;
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    let bytes = &buffer[..count];
                    let split_points = output_tracker
                        .lock()
                        .map_err(|_| anyhow::anyhow!("command tracker lock was poisoned"))?
                        .split_points(bytes);
                    let mut segment_start = 0;
                    for segment_end in split_points.into_iter().chain(std::iter::once(bytes.len()))
                    {
                        let segment = &bytes[segment_start..segment_end];
                        segment_start = segment_end;
                        if segment.is_empty() {
                            continue;
                        }
                        let now = elapsed_us(start).max(last_event_us.saturating_add(1));
                        last_event_us = now;
                        store::append_event(&conn, now, OUTPUT, segment)?;
                        let confirmed = output_tracker
                            .lock()
                            .map_err(|_| anyhow::anyhow!("command tracker lock was poisoned"))?
                            .observe_output(segment, now);
                        persist_confirmed(&conn, confirmed)?;
                    }
                    let mut gate = output_gate
                        .lock()
                        .map_err(|_| anyhow::anyhow!("terminal display lock was poisoned"))?;
                    if gate.menu_active {
                        gate.buffered_output.extend_from_slice(bytes);
                    } else {
                        let stdout = io::stdout();
                        let mut stdout = stdout.lock();
                        stdout.write_all(bytes)?;
                        stdout.flush()?;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                // Linux PTY masters report EIO after the final slave is closed.
                Err(error) if error.raw_os_error() == Some(5) => break,
                Err(error) => return Err(error.into()),
            }
        }
        output_done.store(true, Ordering::Release);
        Ok(())
    });

    let input_path = path.to_owned();
    let data_dir = path
        .parent()
        .context("session database has no parent directory")?
        .to_owned();
    let input_done = Arc::clone(&done);
    let input_gate = Arc::clone(&display_gate);
    let input_tracker = Arc::clone(&command_tracker);
    let interactive = std::io::IsTerminal::is_terminal(&io::stdin());
    let input_thread = thread::spawn(move || -> Result<()> {
        let conn = store::open_event_writer(&input_path)?;
        let mut stdin = io::stdin().lock();
        let mut buffer = [0_u8; 1024];
        let mut command_bytes = Vec::new();
        let mut line_started_us = None;
        let mut line_output_offset = None;
        let mut ctrl_t_armed = false;
        while !input_done.load(Ordering::Acquire) {
            let count = stdin.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            let now = elapsed_us(start);
            let bytes = &buffer[..count];
            let mut forwarded = Vec::with_capacity(count);
            for byte in bytes {
                if interactive && *byte == 0x14 && ctrl_t_armed {
                    forward_input(&mut writer, &forwarded)?;
                    forwarded.clear();
                    store::checkpoint(&input_path, now)?;
                    {
                        let mut gate = input_gate
                            .lock()
                            .map_err(|_| anyhow::anyhow!("terminal display lock was poisoned"))?;
                        gate.menu_active = true;
                    }
                    let menu_result = tui::control_panel(&input_path, &data_dir);
                    {
                        let mut gate = input_gate
                            .lock()
                            .map_err(|_| anyhow::anyhow!("terminal display lock was poisoned"))?;
                        let buffered = std::mem::take(&mut gate.buffered_output);
                        let stdout = io::stdout();
                        let mut stdout = stdout.lock();
                        stdout.write_all(&buffered)?;
                        stdout.flush()?;
                        gate.menu_active = false;
                    }
                    menu_result?;
                    ctrl_t_armed = false;
                    continue;
                }

                if interactive && *byte == 0x14 {
                    ctrl_t_armed = true;
                    continue;
                }
                if ctrl_t_armed {
                    forwarded.push(0x14);
                    if command_bytes.is_empty() {
                        line_started_us = Some(now);
                        line_output_offset = Some(begin_command_line(&input_tracker, &conn, now)?);
                    }
                    command_bytes.push(0x14);
                    ctrl_t_armed = false;
                }

                forwarded.push(*byte);
                match byte {
                    b'\r' | b'\n' => {
                        if !command_bytes.is_empty() {
                            let needle = text::display_input(&command_bytes).into_bytes();
                            if !needle.iter().all(u8::is_ascii_whitespace) {
                                let confirmed = input_tracker
                                    .lock()
                                    .map_err(|_| {
                                        anyhow::anyhow!("command tracker lock was poisoned")
                                    })?
                                    .submit(PendingCommand {
                                        started_us: line_started_us.unwrap_or(now),
                                        submitted_us: now,
                                        output_offset: line_output_offset.unwrap_or_default(),
                                        raw: command_bytes.clone(),
                                        needle,
                                        boundary_on_echo: false,
                                    });
                                persist_confirmed(&conn, confirmed)?;
                            }
                            command_bytes.clear();
                            line_started_us = None;
                            line_output_offset = None;
                        }
                    }
                    0x03 | 0x15 => {
                        command_bytes.clear();
                        line_started_us = None;
                        line_output_offset = None;
                    }
                    _ => {
                        if command_bytes.is_empty() {
                            line_started_us = Some(now);
                            line_output_offset =
                                Some(begin_command_line(&input_tracker, &conn, now)?);
                        }
                        command_bytes.push(*byte);
                    }
                }
            }
            forward_input(&mut writer, &forwarded)?;
        }
        Ok(())
    });

    let mut input_thread = Some(input_thread);
    let mut terminal_size = (cols, rows);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if input_thread
            .as_ref()
            .is_some_and(|handle| handle.is_finished())
        {
            input_thread
                .take()
                .expect("checked above")
                .join()
                .map_err(|_| anyhow::anyhow!("terminal input thread panicked"))??;
        }
        if let Ok((new_cols, new_rows)) = size()
            && terminal_size != (new_cols, new_rows)
        {
            pair.master.resize(PtySize {
                rows: new_rows,
                cols: new_cols,
                pixel_width: 0,
                pixel_height: 0,
            })?;
            terminal_size = (new_cols, new_rows);
        }
        thread::sleep(std::time::Duration::from_millis(25));
    };
    done.store(true, Ordering::Release);
    drop(pair.master);

    output_thread
        .join()
        .map_err(|_| anyhow::anyhow!("PTY output thread panicked"))??;
    // A blocked stdin reader is intentionally detached. It cannot mutate the DB after `done`
    // is observed, and the process is about to leave the recording command.
    if input_thread
        .as_ref()
        .is_some_and(|handle| handle.is_finished())
    {
        input_thread
            .take()
            .expect("checked above")
            .join()
            .map_err(|_| anyhow::anyhow!("terminal input thread panicked"))??;
    }
    store::finish(path, elapsed_us(start))?;
    if store::has_user_activity(path)? {
        let _ = summary::spawn_worker(path);
    }
    Ok(status.exit_code() as i32)
}

fn begin_command_line(
    tracker: &Mutex<CommandTracker>,
    conn: &rusqlite::Connection,
    now_us: i64,
) -> Result<usize> {
    let mut tracker = tracker
        .lock()
        .map_err(|_| anyhow::anyhow!("command tracker lock was poisoned"))?;
    let confirmed = tracker.begin_line(now_us);
    let offset = tracker.output_offset();
    drop(tracker);
    persist_confirmed(conn, confirmed)?;
    Ok(offset)
}

fn forward_input(writer: &mut impl Write, bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    writer.write_all(bytes)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{CommandTracker, PendingCommand};
    use crate::text;

    #[test]
    fn command_display_normalizes_but_raw_input_survives() {
        let raw = b"echp\x7fo";
        assert_eq!(text::display_input(raw), "echo");
        assert_eq!(raw, b"echp\x7fo");
    }

    #[test]
    fn only_echoed_lines_become_commands() {
        let mut tracker = CommandTracker::default();
        let offset = tracker.output_offset();
        assert!(tracker.observe_output(b"echo safe\r\n", 50).is_empty());
        let confirmed = tracker.submit(PendingCommand {
            started_us: 10,
            submitted_us: 100,
            output_offset: offset,
            raw: b"echo safe".to_vec(),
            needle: b"echo safe".to_vec(),
            boundary_on_echo: false,
        });
        assert_eq!(confirmed.len(), 1);
        assert_eq!(confirmed[0].raw, b"echo safe");
    }

    #[test]
    fn rendered_command_is_confirmed_before_result_output() {
        let mut tracker = CommandTracker::default();
        let offset = tracker.output_offset();
        assert!(
            tracker
                .observe_output(b"\r\x1b[2Ksh$ cargo test", 50)
                .is_empty()
        );
        let confirmed = tracker.submit(PendingCommand {
            started_us: 10,
            submitted_us: 100,
            output_offset: offset,
            raw: b"cargo test".to_vec(),
            needle: b"cargo test".to_vec(),
            boundary_on_echo: false,
        });
        assert_eq!(confirmed.len(), 1);
        assert_eq!(confirmed[0].raw, b"cargo test");
    }

    #[test]
    fn pasted_lines_remain_pending_until_the_shell_renders_them() {
        let mut tracker = CommandTracker::default();
        for raw in [b"echo one".as_slice(), b"echo two"] {
            let offset = tracker.output_offset();
            assert!(
                tracker
                    .submit(PendingCommand {
                        started_us: 10,
                        submitted_us: 100,
                        output_offset: offset,
                        raw: raw.to_vec(),
                        needle: raw.to_vec(),
                        boundary_on_echo: false,
                    })
                    .is_empty()
            );
        }
        assert_eq!(
            tracker.split_points(b"echo one\r\none-without-newline sh$ echo two\r\n"),
            vec![34]
        );
        let confirmed = tracker.observe_output(b"echo one\r\none\r\necho two\r\ntwo\r\n", 110);
        assert_eq!(
            confirmed
                .iter()
                .map(|command| command.raw.as_slice())
                .collect::<Vec<_>>(),
            vec![b"echo one".as_slice(), b"echo two".as_slice()]
        );
        assert_eq!(confirmed[0].started_us, 10);
        assert_eq!(confirmed[1].started_us, 110);
    }

    #[test]
    fn hidden_input_is_discarded_before_later_output_can_match_it() {
        let mut tracker = CommandTracker::default();
        let offset = tracker.output_offset();
        assert!(
            tracker
                .submit(PendingCommand {
                    started_us: 10,
                    submitted_us: 100,
                    output_offset: offset,
                    raw: b"secret".to_vec(),
                    needle: b"secret".to_vec(),
                    boundary_on_echo: false,
                })
                .is_empty()
        );

        // Beginning the next line invalidates the unechoed candidate. Even if later output
        // contains the same bytes, the secret can no longer be confirmed or persisted.
        assert!(tracker.begin_line(200).is_empty());
        assert!(tracker.observe_output(b"secret\r\n", 250).is_empty());
    }
}
