use std::{
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
    tui,
};

#[derive(Default)]
struct DisplayGate {
    menu_active: bool,
    buffered_output: Vec<u8>,
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

    let output_path = path.to_owned();
    let output_done = Arc::clone(&done);
    let output_gate = Arc::clone(&display_gate);
    let output_thread = thread::spawn(move || -> Result<()> {
        let conn = store::open_event_writer(&output_path)?;
        let mut buffer = [0_u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    let bytes = &buffer[..count];
                    store::append_event(&conn, elapsed_us(start), OUTPUT, bytes)?;
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
    let interactive = std::io::IsTerminal::is_terminal(&io::stdin());
    let input_thread = thread::spawn(move || -> Result<()> {
        let conn = store::open_event_writer(&input_path)?;
        let mut stdin = io::stdin().lock();
        let mut buffer = [0_u8; 1024];
        let mut command_bytes = Vec::new();
        let mut ordinal = 0_i64;
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
                    forward_input(&conn, &mut writer, now, &forwarded)?;
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
                    command_bytes.push(0x14);
                    ctrl_t_armed = false;
                }

                forwarded.push(*byte);
                match byte {
                    b'\r' | b'\n' => {
                        if !command_bytes.is_empty() {
                            ordinal += 1;
                            store::add_command(&conn, ordinal, now, &command_bytes)?;
                            command_bytes.clear();
                        }
                    }
                    0x03 | 0x15 => command_bytes.clear(),
                    _ => command_bytes.push(*byte),
                }
            }
            forward_input(&conn, &mut writer, now, &forwarded)?;
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
    Ok(status.exit_code() as i32)
}

fn forward_input(
    conn: &rusqlite::Connection,
    writer: &mut impl Write,
    time_us: i64,
    bytes: &[u8],
) -> Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    store::append_event(conn, time_us, INPUT, bytes)?;
    writer.write_all(bytes)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::text;

    #[test]
    fn command_display_normalizes_but_raw_input_survives() {
        let raw = b"echp\x7fo";
        assert_eq!(text::display_input(raw), "echo");
        assert_eq!(raw, b"echp\x7fo");
    }
}
