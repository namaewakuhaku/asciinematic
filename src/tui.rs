use std::{
    fs,
    io::{self, Write},
    path::Path,
    time::{Duration, Instant},
};

use anyhow::Result;
use crossterm::{
    cursor::MoveTo,
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseButton, MouseEventKind,
    },
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

use crate::{record, store, summary, text};
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
const REPLAY_SEEK_US: i64 = 5_000_000;
const EXPORT_STEP_SEPARATOR: &str =
    "────────────────────────────────────────────────────────────────────────";

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
        execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
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
    loop {
        let action = {
            let _raw_mode = RawModeGuard::enter()?;
            let _screen = TerminalGuard::enter_preserving_raw_mode()?;
            let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
            browse_sessions(&mut terminal, data_dir, false, None)?
        };
        match action {
            BrowserExit::Close => return Ok(()),
            BrowserExit::NewSession => {
                let shell = crate::shell::user_shell();
                let (_, code) = record::record_new_session(data_dir, &shell)?;
                if code != 0 {
                    eprintln!("Shell exited with status {code}.");
                }
            }
        }
    }
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
    let mut command_list_area = Rect::default();
    let mut command_list_offset = 0_usize;
    let mut preview_area = Rect::default();
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
        let detail = commands.get(selected).map_or_else(
            || Text::from("No commands have been submitted yet."),
            |item| {
                let snapshot = text::terminal_snapshot(&output);
                let mut lines = vec![
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
                ];
                lines.extend(snapshot.lines().map(|line| Line::raw(line.to_owned())));
                Text::from(lines)
            },
        );
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
            command_list_area = columns[0];
            preview_area = columns[1];
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
            command_list_offset = list_state.offset();

            frame.render_widget(
                Paragraph::new(detail.clone())
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
                        "r replay*  e export  b sessions  ↑/↓ select  q resume  *Space/←/→/q"
                    } else {
                        "r replay*  e export  b/q sessions  ↑/↓ select  *Space/←/→/q"
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
        let preview_scroll_limit = max_text_scroll(&detail, preview_area);
        if scroll > preview_scroll_limit {
            scroll = preview_scroll_limit;
            continue;
        }

        let input_event = event::read()?;
        if let Event::Mouse(mouse) = &input_event {
            if rect_contains(command_list_area, mouse.column, mouse.row) {
                match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        selected = selected.saturating_sub(3);
                        scroll = 0;
                    }
                    MouseEventKind::ScrollDown => {
                        selected = (selected + 3).min(commands.len().saturating_sub(1));
                        scroll = 0;
                    }
                    MouseEventKind::Down(MouseButton::Left) => {
                        if let Some(index) = list_index_at(
                            command_list_area,
                            mouse.column,
                            mouse.row,
                            command_list_offset,
                            1,
                            commands.len(),
                        ) {
                            selected = index;
                            scroll = 0;
                        }
                    }
                    _ => {}
                }
            } else if rect_contains(preview_area, mouse.column, mouse.row) {
                match mouse.kind {
                    MouseEventKind::ScrollUp => scroll = scroll.saturating_sub(3),
                    MouseEventKind::ScrollDown => {
                        scroll = scroll.saturating_add(3).min(preview_scroll_limit);
                    }
                    _ => {}
                }
            }
            continue;
        }
        let Event::Key(key) = input_event else {
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
            KeyCode::PageDown => {
                scroll = scroll.saturating_add(10).min(preview_scroll_limit);
            }
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
                    let _ = browse_sessions(terminal, data_dir, true, Some(path))?;
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

#[derive(Clone, Copy)]
enum BrowserExit {
    Close,
    NewSession,
}

fn browse_sessions(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    data_dir: &Path,
    can_return_to_current: bool,
    protected_session: Option<&Path>,
) -> Result<BrowserExit> {
    let mut session_index = 0_usize;
    let mut scroll = 0_u16;
    let mut status = "Select a session to inspect its complete transcript".to_owned();
    let mut rename_buffer = None::<String>;
    let mut delete_confirmation = None::<std::path::PathBuf>;
    let mut session_list_area = Rect::default();
    let mut session_list_offset = 0_usize;
    let mut transcript_area = Rect::default();

    let action = loop {
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
        let transcript_text = Text::raw(transcript.as_str());

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
            session_list_area = columns[0];
            transcript_area = columns[1];
            let session_items = sessions
                .iter()
                .map(|session| {
                    let is_live =
                        protected_session.is_some_and(|path| path == session.path.as_path());
                    let mut uuid_spans = vec![Span::styled(
                        format!("  {}", session.id),
                        Style::default().fg(BORDER),
                    )];
                    if is_live {
                        uuid_spans.push(Span::styled(
                            " ●",
                            Style::default()
                                .fg(SUCCESS)
                                .add_modifier(Modifier::BOLD | Modifier::SLOW_BLINK),
                        ));
                    }
                    ListItem::new(Text::from(vec![
                        Line::styled(
                            session.name.as_str(),
                            Style::default().fg(FOREGROUND).add_modifier(Modifier::BOLD),
                        ),
                        Line::from(uuid_spans),
                        Line::styled(
                            format!(
                                "  {} · {} · {} cmd",
                                format_timestamp(session.started_at),
                                format_duration(session.duration_us),
                                session.command_count
                            ),
                            Style::default().fg(BORDER),
                        ),
                    ]))
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
            session_list_offset = session_state.offset();

            frame.render_widget(
                Paragraph::new(transcript_text.clone())
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
                    Line::raw(if can_return_to_current {
                        "↑/↓ session  Enter inspect  PgUp/PgDn transcript  n rename"
                    } else {
                        "↑/↓ session  Enter inspect  PgUp/PgDn transcript  n rename  N new"
                    }),
                    Line::raw(if can_return_to_current {
                        "r replay*  e export  d delete  c current  q back  *Space/←/→/q"
                    } else {
                        "r replay*  e export  d delete  q quit  *Space/←/→/q"
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

            if let Some(path) = delete_confirmation.as_deref() {
                let popup = confirmation_popup(frame.area());
                let name = sessions
                    .iter()
                    .find(|session| session.path == path)
                    .map_or("selected session", |session| session.name.as_str());
                frame.render_widget(RatatuiClear, popup);
                frame.render_widget(
                    Paragraph::new(vec![
                        Line::styled(
                            "Permanently delete session?",
                            Style::default().fg(WARNING).add_modifier(Modifier::BOLD),
                        ),
                        Line::raw(name),
                        Line::raw(""),
                        Line::styled(
                            "Press d again to delete · Esc to cancel",
                            Style::default().fg(BORDER),
                        ),
                    ])
                    .style(Style::default().fg(FOREGROUND).bg(PANEL))
                    .block(panel(" Confirm deletion ")),
                    popup,
                );
            }
        })?;
        let transcript_scroll_limit = max_text_scroll(&transcript_text, transcript_area);
        if scroll > transcript_scroll_limit {
            scroll = transcript_scroll_limit;
            continue;
        }

        let input_event = event::read()?;
        if rename_buffer.is_none()
            && delete_confirmation.is_none()
            && let Event::Mouse(mouse) = &input_event
        {
            if rect_contains(session_list_area, mouse.column, mouse.row) {
                match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        session_index = session_index.saturating_sub(1);
                        scroll = 0;
                    }
                    MouseEventKind::ScrollDown => {
                        session_index = (session_index + 1).min(sessions.len().saturating_sub(1));
                        scroll = 0;
                    }
                    MouseEventKind::Down(MouseButton::Left) => {
                        if let Some(index) = list_index_at(
                            session_list_area,
                            mouse.column,
                            mouse.row,
                            session_list_offset,
                            3,
                            sessions.len(),
                        ) {
                            session_index = index;
                            scroll = 0;
                        }
                    }
                    _ => {}
                }
            } else if rect_contains(transcript_area, mouse.column, mouse.row) {
                match mouse.kind {
                    MouseEventKind::ScrollUp => scroll = scroll.saturating_sub(3),
                    MouseEventKind::ScrollDown => {
                        scroll = scroll.saturating_add(3).min(transcript_scroll_limit);
                    }
                    _ => {}
                }
            }
            continue;
        }
        let Event::Key(key) = input_event else {
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

        if let Some(path) = delete_confirmation.clone() {
            match key.code {
                KeyCode::Char('d') => {
                    let name = sessions
                        .iter()
                        .find(|session| session.path == path)
                        .map_or_else(
                            || path.display().to_string(),
                            |session| session.name.clone(),
                        );
                    match store::delete_session(&path) {
                        Ok(()) => {
                            status = format!("Deleted session {name}");
                            scroll = 0;
                        }
                        Err(error) => status = format!("Delete failed: {error}"),
                    }
                    delete_confirmation = None;
                }
                KeyCode::Esc => {
                    delete_confirmation = None;
                    status = "Deletion cancelled".to_owned();
                }
                _ => {}
            }
            continue;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => break BrowserExit::Close,
            KeyCode::Char('c') if can_return_to_current => break BrowserExit::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                session_index = session_index.saturating_sub(1);
                scroll = 0;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                session_index = (session_index + 1).min(sessions.len().saturating_sub(1));
                scroll = 0;
            }
            KeyCode::PageUp => scroll = scroll.saturating_sub(10),
            KeyCode::PageDown => {
                scroll = scroll.saturating_add(10).min(transcript_scroll_limit);
            }
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
            KeyCode::Char('d') => {
                if let Some(session) = selected_session {
                    if protected_session.is_some_and(|path| path == session.path) {
                        status = "The active recording cannot be deleted".to_owned();
                    } else {
                        delete_confirmation = Some(session.path.clone());
                        status = format!("Confirm deletion of {}", session.name);
                    }
                }
            }
            KeyCode::Char('N') => {
                if protected_session.is_some() {
                    status = "Finish the active recording before starting another".to_owned();
                } else {
                    break BrowserExit::NewSession;
                }
            }
            _ => {}
        }
    };
    terminal.clear()?;
    Ok(action)
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

fn confirmation_popup(area: Rect) -> Rect {
    let width = area.width.saturating_sub(4).min(64);
    let height = 8.min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn rect_contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x
        && column < area.x.saturating_add(area.width)
        && row >= area.y
        && row < area.y.saturating_add(area.height)
}

fn max_text_scroll(text: &Text<'_>, area: Rect) -> u16 {
    let width = area.width.saturating_sub(2) as usize;
    let height = area.height.saturating_sub(2) as usize;
    if width == 0 || height == 0 {
        return 0;
    }
    let rendered_rows = text
        .lines
        .iter()
        .map(|line| line.width().max(1).div_ceil(width))
        .sum::<usize>();
    rendered_rows.saturating_sub(height).min(u16::MAX as usize) as u16
}

fn list_index_at(
    area: Rect,
    column: u16,
    row: u16,
    offset: usize,
    item_height: u16,
    item_count: usize,
) -> Option<usize> {
    if area.width < 3
        || area.height < 3
        || item_height == 0
        || column <= area.x
        || column >= area.x.saturating_add(area.width).saturating_sub(1)
        || row <= area.y
        || row >= area.y.saturating_add(area.height).saturating_sub(1)
    {
        return None;
    }
    let visible_row = row.saturating_sub(area.y).saturating_sub(1);
    let index = offset.saturating_add((visible_row / item_height) as usize);
    (index < item_count).then_some(index)
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
    if events.is_empty() {
        let mut stdout = io::stdout().lock();
        stdout.write_all(b"[replay has no output - press any key to return]\r\n")?;
        stdout.flush()?;
        drop(stdout);
        let _ = event::read()?;
        terminal.clear()?;
        return Ok(());
    }

    let start_us = events.first().map(|event| event.0).unwrap_or_default();
    let end_us = events.last().map(|event| event.0).unwrap_or(start_us);
    let mut position_us = start_us;
    let mut event_index = 0;
    let mut paused = false;
    let mut last_tick = Instant::now();
    let mut stopped = false;
    let mut stdout = io::stdout().lock();

    loop {
        let now = Instant::now();
        if !paused {
            let elapsed_us = now
                .duration_since(last_tick)
                .as_micros()
                .min(i64::MAX as u128) as i64;
            position_us = position_us.saturating_add(elapsed_us).min(end_us);
        }
        last_tick = now;
        write_replay_until(&events, &mut event_index, position_us, &mut stdout)?;
        stdout.flush()?;

        if event_index >= events.len() {
            break;
        }
        if !event::poll(Duration::from_millis(25))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                stopped = true;
                break;
            }
            KeyCode::Char(' ') => {
                paused = !paused;
                last_tick = Instant::now();
            }
            KeyCode::Right | KeyCode::Char('l' | 'f') => {
                position_us = seek_replay(position_us, start_us, end_us, REPLAY_SEEK_US);
                last_tick = Instant::now();
            }
            KeyCode::Left | KeyCode::Char('h') => {
                position_us = seek_replay(position_us, start_us, end_us, -REPLAY_SEEK_US);
                redraw_replay(&events, &mut event_index, position_us, &mut stdout)?;
                last_tick = Instant::now();
            }
            KeyCode::Home => {
                position_us = start_us;
                redraw_replay(&events, &mut event_index, position_us, &mut stdout)?;
                last_tick = Instant::now();
            }
            KeyCode::End => {
                position_us = end_us;
                last_tick = Instant::now();
            }
            _ => {}
        }
    }
    if !stopped {
        stdout.write_all(b"\r\n\r\n[replay finished - press any key to return]\r\n")?;
        stdout.flush()?;
    }
    drop(stdout);
    if !stopped {
        let _ = event::read()?;
    }
    terminal.clear()?;
    Ok(())
}

fn seek_replay(position_us: i64, start_us: i64, end_us: i64, delta_us: i64) -> i64 {
    position_us.saturating_add(delta_us).clamp(start_us, end_us)
}

fn write_replay_until(
    events: &[(i64, Vec<u8>)],
    event_index: &mut usize,
    position_us: i64,
    output: &mut impl Write,
) -> Result<()> {
    while let Some((time_us, bytes)) = events.get(*event_index) {
        if *time_us > position_us {
            break;
        }
        output.write_all(bytes)?;
        *event_index += 1;
    }
    Ok(())
}

fn redraw_replay(
    events: &[(i64, Vec<u8>)],
    event_index: &mut usize,
    position_us: i64,
    output: &mut impl Write,
) -> Result<()> {
    execute!(output, MoveTo(0, 0), CrosstermClear(ClearType::All))?;
    *event_index = 0;
    write_replay_until(events, event_index, position_us, output)?;
    output.flush()?;
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
    for (index, item) in commands.iter().enumerate() {
        if index > 0 {
            writeln!(transcript, "{EXPORT_STEP_SEPARATOR}\n")?;
        }
        let input = text::display_input(&item.input);
        writeln!(transcript, "$ {input}")?;
        let snapshot = text::terminal_snapshot(&store::command_output(path, item)?);
        let snapshot = clean_export_snapshot(&snapshot, &input);
        writeln!(transcript, "{snapshot}\n")?;
    }
    Ok(String::from_utf8(transcript).expect("transcript formatter only writes UTF-8"))
}

fn clean_export_snapshot(snapshot: &str, input: &str) -> String {
    let lines = snapshot.lines().collect::<Vec<_>>();
    let first_output = lines
        .iter()
        .position(|line| line.trim() != input)
        .unwrap_or(lines.len());
    lines[first_output..].join("\n").trim_end().to_owned()
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

fn format_timestamp(unix_seconds: i64) -> String {
    let days = unix_seconds.div_euclid(86_400);
    let seconds_of_day = unix_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_date_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = seconds_of_day % 3_600 / 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02} UTC")
}

fn civil_date_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let days = days_since_epoch + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::{
        clean_export_snapshot, encode_base64, format_timestamp, list_index_at, max_text_scroll,
        osc52_sequence, render_commands_text, seek_replay,
    };
    use crate::store::{self, OUTPUT};
    use anyhow::Result;
    use ratatui::{layout::Rect, text::Text};

    #[test]
    fn clipboard_sequence_contains_base64_path() {
        assert_eq!(encode_base64(b"/tmp/a.txt"), "L3RtcC9hLnR4dA==");
        assert_eq!(
            osc52_sequence("/tmp/a.txt"),
            "\u{1b}]52;c;L3RtcC9hLnR4dA==\u{7}"
        );
    }

    #[test]
    fn replay_seek_is_clamped_to_the_recording() {
        assert_eq!(seek_replay(10, 10, 100, -50), 10);
        assert_eq!(seek_replay(50, 10, 100, 25), 75);
        assert_eq!(seek_replay(90, 10, 100, 50), 100);
    }

    #[test]
    fn session_timestamp_is_human_readable_utc() {
        assert_eq!(format_timestamp(0), "1970-01-01 00:00 UTC");
        assert_eq!(format_timestamp(1_704_110_400), "2024-01-01 12:00 UTC");
    }

    #[test]
    fn text_export_removes_leading_command_echoes() {
        assert_eq!(
            clean_export_snapshot("ls\ncompose.yml\nshell$ ", "ls"),
            "compose.yml\nshell$"
        );
        assert_eq!(clean_export_snapshot("exit\nexit", "exit"), "");
    }

    #[test]
    fn mouse_rows_map_to_visible_list_items() {
        let area = Rect::new(10, 5, 30, 14);
        assert_eq!(list_index_at(area, 12, 6, 4, 3, 20), Some(4));
        assert_eq!(list_index_at(area, 12, 9, 4, 3, 20), Some(5));
        assert_eq!(list_index_at(area, 10, 6, 4, 3, 20), None);
        assert_eq!(list_index_at(area, 12, 18, 4, 3, 5), None);
    }

    #[test]
    fn text_scroll_stops_at_wrapped_content_end() {
        let area = Rect::new(0, 0, 12, 5);
        assert_eq!(max_text_scroll(&Text::raw("one\ntwo"), area), 0);
        assert_eq!(
            max_text_scroll(&Text::raw("12345678901\nsecond\nthird"), area),
            1
        );
    }

    #[test]
    fn text_export_separates_command_steps() -> Result<()> {
        let dir =
            std::env::temp_dir().join(format!("asciinematic-export-test-{}", uuid::Uuid::new_v4()));
        let path = store::create_session(&dir, "test", "sh")?;
        let conn = store::open_event_writer(&path)?;
        store::add_command(&conn, 1, 10, b"cd /tmp")?;
        store::append_event(&conn, 11, OUTPUT, b"sh$ ")?;
        store::add_command(&conn, 2, 20, b"pwd")?;
        store::append_event(&conn, 21, OUTPUT, b"/tmp\r\n")?;
        drop(conn);
        store::finish(&path, 30)?;

        let transcript = render_commands_text(&path, &store::commands(&path)?)?;
        assert!(transcript.contains("$ cd /tmp"));
        assert!(transcript.contains(
            "\n────────────────────────────────────────────────────────────────────────\n\n$ pwd"
        ));
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
}
