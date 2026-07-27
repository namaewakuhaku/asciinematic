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
    terminal::{Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{store, text};
use uuid::Uuid;

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

pub fn control_panel(path: &Path, data_dir: &Path) -> Result<()> {
    let session = store::read_session(path)?;
    let commands = store::commands(path)?;
    let _guard = TerminalGuard::enter_preserving_raw_mode()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut selected = commands.len().saturating_sub(1);
    let mut anchor = None;
    let mut scroll = 0_u16;
    let mut status = format!("Recording is live at {}", path.display());

    loop {
        let (range_start, range_end) = selected_range(selected, anchor);
        let output = commands
            .get(selected)
            .map(|item| store::command_output(path, item))
            .transpose()?
            .unwrap_or_default();
        terminal.draw(|frame| {
            let outer = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(8),
                    Constraint::Length(4),
                    Constraint::Length(2),
                ])
                .split(frame.area());
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        " LIVE CONTROL ",
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!(
                        "  {}  commands={}  selected={}..={}",
                        session.name,
                        commands.len(),
                        range_start + 1,
                        range_end + 1
                    )),
                ]))
                .block(Block::default().borders(Borders::ALL)),
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
                        Style::default().fg(Color::LightCyan)
                    } else {
                        Style::default()
                    })
                })
                .collect::<Vec<_>>();
            let list = List::new(items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Current command history "),
                )
                .highlight_style(
                    Style::default()
                        .bg(Color::DarkGray)
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
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
                            Style::default()
                                .fg(Color::Green)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Line::raw(text::display_input(&item.input)),
                        Line::raw(""),
                        Line::styled(
                            format!("OUTPUT ({} raw bytes)", output.len()),
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Line::raw(text::display_bytes(&output)),
                    ])
                },
            );
            frame.render_widget(
                Paragraph::new(detail)
                    .block(Block::default().borders(Borders::ALL).title(" Preview "))
                    .wrap(Wrap { trim: false })
                    .scroll((scroll, 0)),
                columns[1],
            );

            frame.render_widget(
                Paragraph::new(vec![
                    Line::raw("Space range anchor  a all  x clear  w checkpoint  s save range"),
                    Line::raw(
                        "r replay command  e export range  b saved sessions  ↑/↓ select  q resume",
                    ),
                ])
                .block(Block::default().borders(Borders::ALL).title(" Actions ")),
                outer[2],
            );
            frame.render_widget(
                Paragraph::new(status.as_str()).style(Style::default().fg(Color::Yellow)),
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
                let name = format!(
                    "{} commands {}-{}",
                    session.name, first.ordinal, last.ordinal
                );
                let saved = store::save_range_as_session(path, data_dir, first, last, &id, &name)?;
                status = format!("Saved selected range as {}", saved.display());
            }
            KeyCode::Char('e') if !commands.is_empty() => {
                let first = &commands[range_start];
                let last = &commands[range_end];
                let export_path = data_dir.join(format!(
                    "{}-commands-{}-{}.txt",
                    session.id, first.ordinal, last.ordinal
                ));
                export_commands(path, &commands[range_start..=range_end], &export_path)?;
                status = format!("Exported selected range to {}", export_path.display());
            }
            KeyCode::Char('r') if !commands.is_empty() => {
                replay_command(&mut terminal, path, &commands[selected])?;
                status = format!("Replayed command {}", commands[selected].ordinal);
            }
            KeyCode::Char('b') => {
                browse_sessions(&mut terminal, data_dir)?;
                status = "Returned from saved sessions".to_owned();
            }
            _ => {}
        }
    }
    Ok(())
}

fn browse_sessions(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    data_dir: &Path,
) -> Result<()> {
    let sessions = store::list_sessions(data_dir)?;
    let mut session_index = 0_usize;
    let mut command_index = 0_usize;
    let mut scroll = 0_u16;
    let mut status = "Inspecting saved sessions".to_owned();

    loop {
        let selected_session = sessions.get(session_index);
        let commands = selected_session
            .map(|session| store::commands(&session.path))
            .transpose()?
            .unwrap_or_default();
        command_index = command_index.min(commands.len().saturating_sub(1));
        let output = selected_session
            .zip(commands.get(command_index))
            .map(|(session, command)| store::command_output(&session.path, command))
            .transpose()?
            .unwrap_or_default();

        terminal.draw(|frame| {
            let outer = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(8),
                    Constraint::Length(4),
                    Constraint::Length(2),
                ])
                .split(frame.area());
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        " SAVED SESSIONS ",
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Magenta)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!("  {} session(s)", sessions.len())),
                ]))
                .block(Block::default().borders(Borders::ALL)),
                outer[0],
            );

            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(32),
                    Constraint::Percentage(28),
                    Constraint::Percentage(40),
                ])
                .split(outer[1]);
            let session_items = sessions
                .iter()
                .map(|session| {
                    ListItem::new(format!(
                        "{}  {:>8}\n  {} command(s)",
                        session.name,
                        format_duration(session.duration_us),
                        session.command_count
                    ))
                })
                .collect::<Vec<_>>();
            let mut session_state =
                ListState::default().with_selected((!sessions.is_empty()).then_some(session_index));
            frame.render_stateful_widget(
                List::new(session_items)
                    .block(Block::default().borders(Borders::ALL).title(" Sessions "))
                    .highlight_style(
                        Style::default()
                            .bg(Color::DarkGray)
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol("▶ "),
                columns[0],
                &mut session_state,
            );

            let command_items = commands
                .iter()
                .map(|command| {
                    ListItem::new(format!(
                        "{:>3} {}",
                        command.ordinal,
                        text::display_input(&command.input)
                    ))
                })
                .collect::<Vec<_>>();
            let mut command_state =
                ListState::default().with_selected((!commands.is_empty()).then_some(command_index));
            frame.render_stateful_widget(
                List::new(command_items)
                    .block(Block::default().borders(Borders::ALL).title(" Commands "))
                    .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::LightCyan))
                    .highlight_symbol("▶ "),
                columns[1],
                &mut command_state,
            );

            let preview = commands.get(command_index).map_or_else(
                || Text::from("No command selected."),
                |command| {
                    Text::from(vec![
                        Line::styled(
                            text::display_input(&command.input),
                            Style::default()
                                .fg(Color::Green)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Line::raw(""),
                        Line::raw(text::display_bytes(&output)),
                    ])
                },
            );
            frame.render_widget(
                Paragraph::new(preview)
                    .block(Block::default().borders(Borders::ALL).title(" Inspect "))
                    .wrap(Wrap { trim: false })
                    .scroll((scroll, 0)),
                columns[2],
            );
            frame.render_widget(
                Paragraph::new(vec![
                    Line::raw("↑/↓ session  ←/→ command  PgUp/PgDn scroll"),
                    Line::raw("r replay command  R replay session  e export command  q back"),
                ])
                .block(Block::default().borders(Borders::ALL).title(" Actions ")),
                outer[2],
            );
            frame.render_widget(
                Paragraph::new(status.as_str()).style(Style::default().fg(Color::Yellow)),
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
            KeyCode::Up | KeyCode::Char('k') => {
                session_index = session_index.saturating_sub(1);
                command_index = 0;
                scroll = 0;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                session_index = (session_index + 1).min(sessions.len().saturating_sub(1));
                command_index = 0;
                scroll = 0;
            }
            KeyCode::Left | KeyCode::Char('h') => {
                command_index = command_index.saturating_sub(1);
                scroll = 0;
            }
            KeyCode::Right | KeyCode::Char('l') => {
                command_index = (command_index + 1).min(commands.len().saturating_sub(1));
                scroll = 0;
            }
            KeyCode::PageUp => scroll = scroll.saturating_sub(10),
            KeyCode::PageDown => scroll = scroll.saturating_add(10),
            KeyCode::Char('r') => {
                if let Some((session, command)) = selected_session.zip(commands.get(command_index))
                {
                    replay_command(terminal, &session.path, command)?;
                    status = format!("Replayed command {}", command.ordinal);
                }
            }
            KeyCode::Char('R') => {
                if let Some(session) = selected_session {
                    replay_events(terminal, store::output_events(&session.path)?)?;
                    status = format!("Replayed {}", session.name);
                }
            }
            KeyCode::Char('e') => {
                if let Some((session, command)) = selected_session.zip(commands.get(command_index))
                {
                    let export_path =
                        data_dir.join(format!("{}-command-{}.txt", session.id, command.ordinal));
                    export_commands(&session.path, std::slice::from_ref(command), &export_path)?;
                    status = format!("Exported {}", export_path.display());
                }
            }
            _ => {}
        }
    }
    terminal.clear()?;
    Ok(())
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
    execute!(io::stdout(), MoveTo(0, 0), Clear(ClearType::All))?;
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

fn export_commands(path: &Path, commands: &[store::CommandItem], export_path: &Path) -> Result<()> {
    let mut exported = Vec::new();
    for item in commands {
        writeln!(exported, "$ {}", text::display_input(&item.input))?;
        exported.extend(text::strip_terminal_controls(&store::command_output(
            path, item,
        )?));
        if !exported.ends_with(b"\n") {
            exported.push(b'\n');
        }
        exported.push(b'\n');
    }
    fs::write(export_path, exported)?;
    Ok(())
}

pub fn format_duration(microseconds: i64) -> String {
    let seconds = microseconds.max(0) as f64 / 1_000_000.0;
    if seconds < 60.0 {
        format!("{seconds:.2}s")
    } else {
        format!("{}:{:05.2}", seconds as u64 / 60, seconds % 60.0)
    }
}
