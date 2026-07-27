use std::{
    fs,
    io::{self, Write},
    path::Path,
    thread,
    time::Duration,
};

use anyhow::Result;
use crossterm::{
    cursor::MoveTo,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{
        Clear as CrosstermClear, ClearType, EnterAlternateScreen, LeaveAlternateScreen,
        disable_raw_mode, enable_raw_mode,
    },
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear as RatatuiClear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{store, summary, text};
use uuid::Uuid;

const BACKGROUND: Color = Color::Rgb(10, 14, 22);
const PANEL: Color = Color::Rgb(18, 25, 38);
const FOREGROUND: Color = Color::Rgb(241, 245, 249);
const BORDER: Color = Color::Rgb(100, 116, 139);
const ACCENT: Color = Color::Rgb(45, 212, 191);
const SECONDARY: Color = Color::Rgb(216, 180, 254);
const SUCCESS: Color = Color::Rgb(134, 239, 172);
const WARNING: Color = Color::Rgb(253, 224, 71);
const SELECTED: Color = Color::Rgb(30, 64, 175);

fn base_style() -> Style {
    Style::default().fg(FOREGROUND).bg(BACKGROUND)
}

fn panel<'a>(title: &'a str) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .title(title)
        .style(Style::default().fg(FOREGROUND).bg(PANEL))
        .border_style(Style::default().fg(BORDER))
        .title_style(Style::default().fg(SECONDARY).add_modifier(Modifier::BOLD))
}

fn selected_style() -> Style {
    Style::default()
        .fg(Color::White)
        .bg(SELECTED)
        .add_modifier(Modifier::BOLD)
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter_preserving_raw_mode() -> Result<Self> {
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

struct RawModeGuard;

impl RawModeGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

pub fn sessions_menu(data_dir: &Path) -> Result<()> {
    let _raw_mode = RawModeGuard::enter()?;
    let _screen = TerminalGuard::enter_preserving_raw_mode()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    browse_sessions(&mut terminal, data_dir, false)
}

pub fn control_panel(path: &Path, data_dir: &Path) -> Result<()> {
    let _guard = TerminalGuard::enter_preserving_raw_mode()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    session_control_panel(&mut terminal, path, data_dir, true)
}

fn session_control_panel(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    path: &Path,
    data_dir: &Path,
    is_live: bool,
) -> Result<()> {
    let mut session = store::read_session(path)?;
    let commands = store::commands(path)?;
    let mut selected = commands.len().saturating_sub(1);
    let mut anchor = None;
    let mut scroll = 0_u16;
    let mut status = if is_live {
        format!("Recording is live at {}", path.display())
    } else {
        format!("Inspecting saved session {}", session.name)
    };

    loop {
        let (range_start, range_end) = selected_range(selected, anchor);
        let output = commands
            .get(selected)
            .map(|item| store::command_output(path, item))
            .transpose()?
            .unwrap_or_default();
        terminal.draw(|frame| {
            frame.render_widget(Block::default().style(base_style()), frame.area());
            let outer = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(8),
                    Constraint::Length(4),
                    Constraint::Length(2),
                    Constraint::Length(7),
                ])
                .split(frame.area());
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        if is_live {
                            " LIVE CONTROL "
                        } else {
                            " SESSION CONTROL "
                        },
                        Style::default()
                            .fg(Color::Black)
                            .bg(ACCENT)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!(
                            "  {}  [{}]  commands={}  selected={}..={}",
                            session.name,
                            &session.id[..session.id.len().min(8)],
                            commands.len(),
                            range_start + 1,
                            range_end + 1
                        ),
                        Style::default().fg(FOREGROUND),
                    ),
                ]))
                .block(panel("")),
                outer[0],
            );

            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
                .split(outer[1]);
            let items = commands
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    let in_range = index >= range_start && index <= range_end;
                    let marker = if in_range { "●" } else { " " };
                    ListItem::new(format!(
                        "{marker} {:>3} {:>8}  {}",
                        item.ordinal,
                        format_duration(item.started_us),
                        text::display_input(&item.input)
                    ))
                    .style(if in_range {
                        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(FOREGROUND)
                    })
                })
                .collect::<Vec<_>>();
            let list = List::new(items)
                .block(panel(" Current command history "))
                .highlight_style(selected_style())
                .highlight_symbol("▶ ");
            let mut list_state =
                ListState::default().with_selected((!commands.is_empty()).then_some(selected));
            frame.render_stateful_widget(list, columns[0], &mut list_state);

            let detail = commands.get(selected).map_or_else(
                || Text::from("No commands have been submitted yet."),
                |item| {
                    Text::from(vec![
                        Line::styled(
                            format!("COMMAND {}", item.ordinal),
                            Style::default().fg(SUCCESS).add_modifier(Modifier::BOLD),
                        ),
                        Line::raw(text::display_input(&item.input)),
                        Line::raw(""),
                        Line::styled(
                            format!("SNAPSHOT ({} recorded bytes)", output.len()),
                            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                        ),
                        Line::raw(text::terminal_snapshot(&output)),
                    ])
                },
            );
            frame.render_widget(
                Paragraph::new(detail)
                    .style(Style::default().fg(FOREGROUND).bg(PANEL))
                    .block(panel(" Preview "))
                    .wrap(Wrap { trim: false })
                    .scroll((scroll, 0)),
                columns[1],
            );

            frame.render_widget(
                Paragraph::new(if session.summary.is_empty() {
                    summary::placeholder()
                } else {
                    session.summary.as_str()
                })
                .style(Style::default().fg(FOREGROUND).bg(PANEL))
                .block(panel(" Session summary "))
                .wrap(Wrap { trim: false }),
                outer[4],
            );
            frame.render_widget(
                Paragraph::new(vec![
                    Line::raw("Space range anchor  a all  x clear  w checkpoint  s save range"),
                    Line::raw(if is_live {
                        "r replay command  e export range  b saved sessions  ↑/↓ select  q resume"
                    } else {
                        "r replay command  e export range  b/q session list  ↑/↓ select"
                    }),
                ])
                .style(Style::default().fg(FOREGROUND).bg(PANEL))
                .block(panel(" Actions ")),
                outer[2],
            );
            frame.render_widget(
                Paragraph::new(status.as_str()).style(Style::default().fg(WARNING).bg(BACKGROUND)),
                outer[3],
            );
        })?;

        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => break,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
            KeyCode::Up | KeyCode::Char('k') => {
                selected = selected.saturating_sub(1);
                scroll = 0;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                selected = (selected + 1).min(commands.len().saturating_sub(1));
                scroll = 0;
            }
            KeyCode::PageUp => scroll = scroll.saturating_sub(10),
            KeyCode::PageDown => scroll = scroll.saturating_add(10),
            KeyCode::Char(' ') if !commands.is_empty() => {
                anchor = if anchor == Some(selected) {
                    None
                } else {
                    Some(selected)
                };
            }
            KeyCode::Char('a') if !commands.is_empty() => {
                anchor = Some(0);
                selected = commands.len() - 1;
            }
            KeyCode::Char('x') => anchor = None,
            KeyCode::Char('w') => {
                store::checkpoint(path, store::latest_event_time(path)?)?;
                status = format!("Checkpoint saved: {}", path.display());
            }
            KeyCode::Char('s') if !commands.is_empty() => {
                let first = &commands[range_start];
                let last = &commands[range_end];
                let id = Uuid::new_v4().simple().to_string();
                let saved = store::save_range_as_session(path, data_dir, first, last, &id)?;
                let _ = summary::spawn_worker(&saved);
                status = format!("Saved selected range as {}", saved.display());
            }
            KeyCode::Char('e') if !commands.is_empty() => {
                let first = &commands[range_start];
                let last = &commands[range_end];
                let export_path = data_dir.join(format!(
                    "{}-commands-{}-{}.txt",
                    session.id, first.ordinal, last.ordinal
                ));
                let export_path =
                    export_commands(path, &commands[range_start..=range_end], &export_path)?;
                status = format!(
                    "Exported; path copied to clipboard: {}",
                    export_path.display()
                );
            }
            KeyCode::Char('r') if !commands.is_empty() => {
                replay_command(terminal, path, &commands[selected])?;
                status = format!("Replayed command {}", commands[selected].ordinal);
            }
            KeyCode::Char('b') => {
                if is_live {
                    browse_sessions(terminal, data_dir, true)?;
                    session = store::read_session(path)?;
                    status = "Returned to current-session controls".to_owned();
                } else {
                    break;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn browse_sessions(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    data_dir: &Path,
    can_return_to_current: bool,
) -> Result<()> {
    let mut session_index = 0_usize;
    let mut scroll = 0_u16;
    let mut status = "Select a session to inspect its complete transcript".to_owned();
    let mut rename_buffer = None::<String>;

    loop {
        let sessions = store::list_sessions(data_dir)?;
        session_index = session_index.min(sessions.len().saturating_sub(1));
        let selected_session = sessions.get(session_index);
        let commands = selected_session
            .map(|session| store::commands(&session.path))
            .transpose()?
            .unwrap_or_default();
        let transcript = selected_session
            .map(|session| render_commands_text(&session.path, &commands))
            .transpose()?
            .unwrap_or_else(|| "No session selected.".to_owned());

        terminal.draw(|frame| {
            frame.render_widget(Block::default().style(base_style()), frame.area());
            let outer = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(8),
                    Constraint::Length(4),
                    Constraint::Length(2),
                    Constraint::Length(7),
                ])
                .split(frame.area());
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        " SAVED SESSIONS ",
                        Style::default()
                            .fg(Color::Black)
                            .bg(SECONDARY)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  {} session(s)", sessions.len()),
                        Style::default().fg(FOREGROUND),
                    ),
                ]))
                .block(panel("")),
                outer[0],
            );

            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(34), Constraint::Percentage(66)])
                .split(outer[1]);
            let session_items = sessions
                .iter()
                .map(|session| {
                    ListItem::new(format!(
                        "{}\n  {}  {:>8}  {} command(s)",
                        session.name,
                        &session.id[..session.id.len().min(8)],
                        format_duration(session.duration_us),
                        session.command_count
                    ))
                })
                .collect::<Vec<_>>();
            let mut session_state =
                ListState::default().with_selected((!sessions.is_empty()).then_some(session_index));
            frame.render_stateful_widget(
                List::new(session_items)
                    .block(panel(" Sessions "))
                    .highlight_style(selected_style())
                    .highlight_symbol("▶ "),
                columns[0],
                &mut session_state,
            );

            frame.render_widget(
                Paragraph::new(transcript.as_str())
                    .style(Style::default().fg(FOREGROUND).bg(PANEL))
                    .block(panel(" Full transcript "))
                    .wrap(Wrap { trim: false })
                    .scroll((scroll, 0)),
                columns[1],
            );
            frame.render_widget(
                Paragraph::new(selected_session.map_or("No session selected.", |session| {
                    if session.summary.is_empty() {
                        summary::placeholder()
                    } else {
                        session.summary.as_str()
                    }
                }))
                .style(Style::default().fg(FOREGROUND).bg(PANEL))
                .block(panel(" Session summary "))
                .wrap(Wrap { trim: false }),
                outer[4],
            );
            frame.render_widget(
                Paragraph::new(vec![
                    Line::raw("↑/↓ session  Enter inspect/actions  PgUp/PgDn transcript  n rename"),
                    Line::raw(if can_return_to_current {
                        "r replay session  e export session  c current-session controls  q back"
                    } else {
                        "r replay session  e export session  q quit"
                    }),
                ])
                .style(Style::default().fg(FOREGROUND).bg(PANEL))
                .block(panel(" Actions ")),
                outer[2],
            );
            frame.render_widget(
                Paragraph::new(status.as_str()).style(Style::default().fg(WARNING).bg(BACKGROUND)),
                outer[3],
            );

            if let Some(buffer) = rename_buffer.as_deref() {
                let popup = rename_popup(frame.area());
                frame.render_widget(RatatuiClear, popup);
                frame.render_widget(
                    Paragraph::new(vec![
                        Line::styled(
                            "Rename selected session",
                            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                        ),
                        Line::raw(format!(
                            "Current: {}",
                            selected_session.map_or("", |session| session.name.as_str())
                        )),
                        Line::raw(buffer),
                        Line::styled(
                            "Enter save · Esc cancel · Backspace edit",
                            Style::default().fg(BORDER),
                        ),
                    ])
                    .style(Style::default().fg(FOREGROUND).bg(PANEL))
                    .block(panel(" Rename ")),
                    popup,
                );
            }
        })?;

        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        if let Some(buffer) = rename_buffer.as_mut() {
            match key.code {
                KeyCode::Esc => {
                    rename_buffer = None;
                    status = "Rename cancelled".to_owned();
                }
                KeyCode::Enter => {
                    if let Some(session) = selected_session {
                        match store::rename_session(&session.path, buffer) {
                            Ok(name) => status = format!("Renamed session to {name}"),
                            Err(error) => status = format!("Rename failed: {error}"),
                        }
                    }
                    rename_buffer = None;
                }
                KeyCode::Backspace => {
                    buffer.pop();
                }
                KeyCode::Char(character)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    buffer.push(character);
                }
                _ => {}
            }
            continue;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => break,
            KeyCode::Char('c') if can_return_to_current => break,
            KeyCode::Up | KeyCode::Char('k') => {
                session_index = session_index.saturating_sub(1);
                scroll = 0;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                session_index = (session_index + 1).min(sessions.len().saturating_sub(1));
                scroll = 0;
            }
            KeyCode::PageUp => scroll = scroll.saturating_sub(10),
            KeyCode::PageDown => scroll = scroll.saturating_add(10),
            KeyCode::Enter => {
                if let Some(session) = selected_session {
                    session_control_panel(terminal, &session.path, data_dir, false)?;
                    scroll = 0;
                    status = format!("Returned from {}", session.name);
                }
            }
            KeyCode::Char('r') => {
                if let Some(session) = selected_session {
                    replay_events(terminal, store::output_events(&session.path)?)?;
                    status = format!("Replayed {}", session.name);
                }
            }
            KeyCode::Char('e') => {
                if let Some(session) = selected_session {
                    let export_path = data_dir.join(format!("{}-transcript.txt", session.id));
                    let export_path = export_commands(&session.path, &commands, &export_path)?;
                    status = format!(
                        "Exported; path copied to clipboard: {}",
                        export_path.display()
                    );
                }
            }
            KeyCode::Char('n') if selected_session.is_some() => {
                rename_buffer = Some(String::new());
                status = "Editing session name".to_owned();
            }
            _ => {}
        }
    }
    terminal.clear()?;
    Ok(())
}

fn rename_popup(area: Rect) -> Rect {
    let width = area.width.saturating_sub(4).min(72);
    let height = 6.min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn selected_range(selected: usize, anchor: Option<usize>) -> (usize, usize) {
    let anchor = anchor.unwrap_or(selected);
    (selected.min(anchor), selected.max(anchor))
}

fn replay_command(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    path: &Path,
    item: &store::CommandItem,
) -> Result<()> {
    replay_events(terminal, store::command_output_events(path, item)?)
}

fn replay_events(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    events: Vec<(i64, Vec<u8>)>,
) -> Result<()> {
    terminal.clear()?;
    execute!(io::stdout(), MoveTo(0, 0), CrosstermClear(ClearType::All))?;
    let mut previous_us = events.first().map(|event| event.0).unwrap_or_default();
    let mut stdout = io::stdout().lock();
    for (time_us, bytes) in events {
        let delay = time_us.saturating_sub(previous_us);
        if delay > 0 {
            thread::sleep(Duration::from_micros(delay as u64));
        }
        stdout.write_all(&bytes)?;
        stdout.flush()?;
        previous_us = time_us;
    }
    stdout.write_all(b"\r\n\r\n[replay finished - press any key to return]\r\n")?;
    stdout.flush()?;
    drop(stdout);
    let _ = event::read()?;
    terminal.clear()?;
    Ok(())
}

fn export_commands(
    path: &Path,
    commands: &[store::CommandItem],
    export_path: &Path,
) -> Result<std::path::PathBuf> {
    fs::write(export_path, render_commands_text(path, commands)?)?;
    let absolute_path = fs::canonicalize(export_path).unwrap_or_else(|_| export_path.to_owned());
    copy_path_to_clipboard(&absolute_path)?;
    Ok(absolute_path)
}

fn render_commands_text(path: &Path, commands: &[store::CommandItem]) -> Result<String> {
    if commands.is_empty() {
        return Ok("No commands recorded.\n".to_owned());
    }
    let mut transcript = Vec::new();
    for item in commands {
        writeln!(transcript, "$ {}", text::display_input(&item.input))?;
        let snapshot = text::terminal_snapshot(&store::command_output(path, item)?);
        writeln!(transcript, "{snapshot}\n")?;
    }
    Ok(String::from_utf8(transcript).expect("transcript formatter only writes UTF-8"))
}

fn copy_path_to_clipboard(path: &Path) -> Result<()> {
    let sequence = osc52_sequence(&path.to_string_lossy());
    let mut stdout = io::stdout().lock();
    stdout.write_all(sequence.as_bytes())?;
    stdout.flush()?;
    Ok(())
}

fn osc52_sequence(value: &str) -> String {
    format!("\x1b]52;c;{}\x07", encode_base64(value.as_bytes()))
}

fn encode_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or_default();
        let third = chunk.get(2).copied().unwrap_or_default();
        encoded.push(ALPHABET[(first >> 2) as usize] as char);
        encoded.push(ALPHABET[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        encoded.push(if chunk.len() > 1 {
            ALPHABET[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            ALPHABET[(third & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    encoded
}

pub fn format_duration(microseconds: i64) -> String {
    let seconds = microseconds.max(0) as f64 / 1_000_000.0;
    if seconds < 60.0 {
        format!("{seconds:.2}s")
    } else {
        format!("{}:{:05.2}", seconds as u64 / 60, seconds % 60.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{encode_base64, osc52_sequence};

    #[test]
    fn clipboard_sequence_contains_base64_path() {
        assert_eq!(encode_base64(b"/tmp/a.txt"), "L3RtcC9hLnR4dA==");
        assert_eq!(
            osc52_sequence("/tmp/a.txt"),
            "\u{1b}]52;c;L3RtcC9hLnR4dA==\u{7}"
        );
    }
}
