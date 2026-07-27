use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use directories::BaseDirs;
use rusqlite::{Connection, OpenFlags, params};

use crate::text;

pub const OUTPUT: i64 = 0;
pub const INPUT: i64 = 1;
pub const APPLICATION_ID: i64 = 0x4153_4349; // "ASCI"
pub const FORMAT_VERSION: i64 = 2;
pub const UNTITLED_TITLE: &str = "Untitled";

#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub started_at: i64,
    pub duration_us: i64,
    pub command_count: i64,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct CommandItem {
    pub ordinal: i64,
    pub started_us: i64,
    pub ended_us: i64,
    pub input: Vec<u8>,
}

pub fn default_data_dir() -> Result<PathBuf> {
    let base = BaseDirs::new().context("could not determine your home directory")?;
    Ok(base.config_dir().join("asciinematic"))
}

pub fn create_session(dir: &Path, id: &str, program: &str) -> Result<PathBuf> {
    fs::create_dir_all(dir)
        .with_context(|| format!("failed to create session directory {}", dir.display()))?;
    let path = dir.join(id);
    anyhow::ensure!(!path.exists(), "session id collision at {}", path.display());
    let conn = Connection::open(&path)?;
    conn.pragma_update(None, "application_id", APPLICATION_ID)?;
    conn.pragma_update(None, "user_version", FORMAT_VERSION)?;
    conn.execute_batch(
        "
        PRAGMA journal_mode = DELETE;
        PRAGMA synchronous = FULL;
        CREATE TABLE metadata (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE TABLE events (
            id         INTEGER PRIMARY KEY,
            time_us    INTEGER NOT NULL,
            direction  INTEGER NOT NULL CHECK(direction IN (0, 1)),
            data       BLOB NOT NULL
        );
        CREATE INDEX events_time ON events(time_us, id);
        CREATE TABLE commands (
            id          INTEGER PRIMARY KEY,
            ordinal     INTEGER NOT NULL UNIQUE,
            started_us  INTEGER NOT NULL,
            ended_us    INTEGER,
            input       BLOB NOT NULL
        );
        ",
    )?;
    let started_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string();
    let format_version = FORMAT_VERSION.to_string();
    let metadata: [(&str, &str); 7] = [
        ("format_version", format_version.as_str()),
        ("id", id),
        ("name", UNTITLED_TITLE),
        ("summary", ""),
        ("program", program),
        ("started_at", &started_at),
        ("duration_us", "0"),
    ];
    for (key, value) in metadata {
        conn.execute(
            "INSERT INTO metadata(key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
    }
    Ok(path)
}

pub fn open_event_writer(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    Ok(conn)
}

pub fn append_event(conn: &Connection, time_us: i64, direction: i64, data: &[u8]) -> Result<()> {
    conn.execute(
        "INSERT INTO events(time_us, direction, data) VALUES (?1, ?2, ?3)",
        params![time_us, direction, data],
    )?;
    Ok(())
}

pub fn add_command(conn: &Connection, ordinal: i64, started_us: i64, input: &[u8]) -> Result<()> {
    conn.execute(
        "UPDATE commands SET ended_us = ?1 WHERE ended_us IS NULL",
        [started_us],
    )?;
    conn.execute(
        "INSERT INTO commands(ordinal, started_us, input) VALUES (?1, ?2, ?3)",
        params![ordinal, started_us, input],
    )?;
    Ok(())
}

pub fn finish(path: &Path, duration_us: i64) -> Result<()> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute(
        "UPDATE commands SET ended_us = ?1 WHERE ended_us IS NULL",
        [duration_us],
    )?;
    conn.execute(
        "UPDATE metadata SET value = ?1 WHERE key = 'duration_us'",
        [duration_us.to_string()],
    )?;
    // Convert older WAL recordings to the single-file format after all event writers stop.
    conn.pragma_update(None, "journal_mode", "DELETE")?;
    drop(conn);
    remove_sqlite_sidecars(path)?;
    Ok(())
}

pub fn checkpoint(path: &Path, duration_us: i64) -> Result<()> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute(
        "UPDATE metadata SET value = ?1 WHERE key = 'duration_us'",
        [duration_us.to_string()],
    )?;
    Ok(())
}

pub fn set_summary(path: &Path, summary: &str) -> Result<()> {
    let conn = open_existing(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute(
        "INSERT INTO metadata(key, value) VALUES ('summary', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [summary],
    )?;
    Ok(())
}

pub fn set_generated_title(path: &Path, requested_title: &str) -> Result<bool> {
    let title = clean_session_name(requested_title, 60)?;
    let conn = open_existing(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    let updated = conn.execute(
        "UPDATE metadata SET value = ?1
         WHERE key = 'name'
           AND (
               trim(value) = ''
               OR value = ?2
               OR value = (SELECT value FROM metadata WHERE key = 'id')
           )",
        params![title.as_str(), UNTITLED_TITLE],
    )?;
    if updated > 0 {
        return Ok(true);
    }
    let inserted = conn.execute(
        "INSERT INTO metadata(key, value)
         SELECT 'name', ?1
         WHERE NOT EXISTS (SELECT 1 FROM metadata WHERE key = 'name')",
        [title.as_str()],
    )?;
    Ok(inserted > 0)
}

pub fn rename_session(path: &Path, requested_name: &str) -> Result<String> {
    let name = clean_session_name(requested_name, 80)?;
    let conn = open_existing(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute(
        "INSERT INTO metadata(key, value) VALUES ('name', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [name.as_str()],
    )?;
    Ok(name)
}

fn clean_session_name(requested_name: &str, max_characters: usize) -> Result<String> {
    let name = requested_name
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .take(max_characters)
        .collect::<String>();
    let name = name.trim();
    anyhow::ensure!(!name.is_empty(), "session name cannot be empty");
    Ok(name.to_owned())
}

fn open_existing(path: &Path) -> Result<Connection> {
    Ok(Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE,
    )?)
}

pub fn delete_session(path: &Path) -> Result<()> {
    // Validate the target before removing it so an unrelated file in the data
    // directory cannot be silently deleted through this API.
    read_session(path)?;
    remove_sqlite_sidecars(path)?;
    fs::remove_file(path)
        .with_context(|| format!("failed to delete session {}", path.display()))?;
    Ok(())
}

/// Delete a recording that contains no activity beyond leaving the shell.
///
/// Shell startup prompts are output events, so event count alone cannot distinguish an
/// empty recording. A submitted command other than `exit`/`logout` makes it worth keeping.
pub fn has_user_activity(path: &Path) -> Result<bool> {
    Ok(commands(path)?.iter().any(|command| {
        let input = text::display_input(&command.input);
        !matches!(
            input.split_whitespace().next(),
            None | Some("exit" | "logout")
        )
    }))
}

pub fn discard_if_empty(path: &Path) -> Result<bool> {
    if has_user_activity(path)? {
        return Ok(false);
    }

    remove_sqlite_sidecars(path)?;
    if path.exists() {
        fs::remove_file(path)
            .with_context(|| format!("failed to remove empty session {}", path.display()))?;
    }
    Ok(true)
}

fn remove_sqlite_sidecars(path: &Path) -> Result<()> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(suffix);
        let sidecar = PathBuf::from(sidecar);
        if sidecar.exists() {
            fs::remove_file(&sidecar)
                .with_context(|| format!("failed to remove {}", sidecar.display()))?;
        }
    }
    Ok(())
}

pub fn list_sessions(dir: &Path) -> Result<Vec<Session>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut sessions = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if !path.is_file() {
            continue;
        }
        if let Ok(session) = read_session(&path) {
            sessions.push(session);
        }
    }
    sessions.sort_by_key(|s| std::cmp::Reverse(s.started_at));
    Ok(sessions)
}

pub fn read_session(path: &Path) -> Result<Session> {
    let conn = Connection::open(path)?;
    let application_id: i64 = conn.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    anyhow::ensure!(
        application_id == APPLICATION_ID || application_id == 0,
        "{} is not an asciinematic session",
        path.display()
    );
    let user_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    anyhow::ensure!(
        user_version <= FORMAT_VERSION,
        "{} uses unsupported session format version {}",
        path.display(),
        user_version
    );
    let meta = |key: &str| -> Result<String> {
        conn.query_row("SELECT value FROM metadata WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .with_context(|| format!("missing {key} metadata"))
    };
    let command_count = conn.query_row("SELECT count(*) FROM commands", [], |row| row.get(0))?;
    let summary = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'summary'",
            [],
            |row| row.get(0),
        )
        .unwrap_or_default();
    let id = meta("id")?;
    let stored_name = conn
        .query_row("SELECT value FROM metadata WHERE key = 'name'", [], |row| {
            row.get(0)
        })
        .unwrap_or_else(|_| id.clone());
    let name = if stored_name.trim().is_empty() || stored_name == id {
        UNTITLED_TITLE.to_owned()
    } else {
        stored_name
    };
    Ok(Session {
        id,
        name,
        summary,
        started_at: meta("started_at")?.parse().unwrap_or_default(),
        duration_us: meta("duration_us")?.parse().unwrap_or_default(),
        command_count,
        path: path.to_owned(),
    })
}

pub fn commands(path: &Path) -> Result<Vec<CommandItem>> {
    let conn = Connection::open(path)?;
    let duration: i64 = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'duration_us'",
            [],
            |row| row.get::<_, String>(0),
        )?
        .parse()
        .unwrap_or_default();
    let mut stmt = conn.prepare(
        "SELECT ordinal, started_us, coalesce(ended_us, ?1), input
         FROM commands ORDER BY ordinal",
    )?;
    let rows = stmt.query_map([duration], |row| {
        Ok(CommandItem {
            ordinal: row.get(0)?,
            started_us: row.get(1)?,
            ended_us: row.get(2)?,
            input: row.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn command_output(path: &Path, item: &CommandItem) -> Result<Vec<u8>> {
    let conn = Connection::open(path)?;
    let mut stmt = conn.prepare(
        "SELECT data FROM events
         WHERE direction = ?1 AND time_us >= ?2 AND time_us < ?3
         ORDER BY time_us, id",
    )?;
    let chunks = stmt.query_map(params![OUTPUT, item.started_us, item.ended_us], |row| {
        row.get::<_, Vec<u8>>(0)
    })?;
    let mut bytes = Vec::new();
    for chunk in chunks {
        bytes.extend(chunk?);
    }
    Ok(bytes)
}

pub fn command_output_events(path: &Path, item: &CommandItem) -> Result<Vec<(i64, Vec<u8>)>> {
    let conn = Connection::open(path)?;
    let mut stmt = conn.prepare(
        "SELECT time_us, data FROM events
         WHERE direction = ?1 AND time_us >= ?2 AND time_us < ?3
         ORDER BY time_us, id",
    )?;
    let rows = stmt.query_map(params![OUTPUT, item.started_us, item.ended_us], |row| {
        Ok((row.get(0)?, row.get(1)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn latest_event_time(path: &Path) -> Result<i64> {
    let conn = Connection::open(path)?;
    Ok(
        conn.query_row("SELECT coalesce(max(time_us), 0) FROM events", [], |row| {
            row.get(0)
        })?,
    )
}

pub fn output_events(path: &Path) -> Result<Vec<(i64, Vec<u8>)>> {
    let conn = Connection::open(path)?;
    let mut stmt =
        conn.prepare("SELECT time_us, data FROM events WHERE direction = ?1 ORDER BY time_us, id")?;
    let rows = stmt.query_map([OUTPUT], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn save_range_as_session(
    source: &Path,
    dir: &Path,
    first: &CommandItem,
    last: &CommandItem,
    id: &str,
) -> Result<PathBuf> {
    let source_conn = Connection::open(source)?;
    let program: String = source_conn.query_row(
        "SELECT value FROM metadata WHERE key = 'program'",
        [],
        |row| row.get(0),
    )?;
    let destination = create_session(dir, id, &program)?;
    let destination_conn = open_event_writer(&destination)?;
    let range_start = first.started_us;
    let range_end = last.ended_us.max(range_start);

    let mut event_stmt = source_conn.prepare(
        "SELECT time_us, direction, data FROM events
         WHERE time_us >= ?1 AND time_us < ?2 ORDER BY time_us, id",
    )?;
    let events = event_stmt.query_map(params![range_start, range_end], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, Vec<u8>>(2)?,
        ))
    })?;
    for event in events {
        let (time_us, direction, data) = event?;
        append_event(
            &destination_conn,
            time_us.saturating_sub(range_start),
            direction,
            &data,
        )?;
    }

    let selected = commands(source)?
        .into_iter()
        .filter(|item| item.ordinal >= first.ordinal && item.ordinal <= last.ordinal);
    for (index, item) in selected.enumerate() {
        add_command(
            &destination_conn,
            index as i64 + 1,
            item.started_us.saturating_sub(range_start),
            &item.input,
        )?;
    }
    drop(destination_conn);
    finish(&destination, range_end.saturating_sub(range_start))?;
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_output_obeys_timeline_boundaries() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("asciinematic-test-{}", uuid::Uuid::new_v4()));
        let path = create_session(&dir, "test", "sh")?;
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("test")
        );
        assert!(path.extension().is_none());
        let format = Connection::open(&path)?;
        assert_eq!(
            format.query_row("PRAGMA application_id", [], |row| row.get::<_, i64>(0))?,
            APPLICATION_ID
        );
        assert_eq!(
            format.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?,
            FORMAT_VERSION
        );
        assert_eq!(
            format.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))?,
            "delete"
        );
        drop(format);
        let conn = open_event_writer(&path)?;
        assert_eq!(
            conn.query_row("PRAGMA synchronous", [], |row| row.get::<_, i64>(0))?,
            2
        );
        add_command(&conn, 1, 10, b"echo one")?;
        append_event(&conn, 11, OUTPUT, b"one\r\n")?;
        add_command(&conn, 2, 20, b"echo two")?;
        append_event(&conn, 21, OUTPUT, b"two\r\n")?;
        drop(conn);
        finish(&path, 30)?;
        assert!(!dir.join("test-wal").exists());
        assert!(!dir.join("test-shm").exists());
        set_summary(&path, "Built the project.\nAll tests passed.")?;
        assert_eq!(
            read_session(&path)?.summary,
            "Built the project.\nAll tests passed."
        );
        assert_eq!(read_session(&path)?.name, UNTITLED_TITLE);
        assert!(set_generated_title(&path, "Build and Test Project")?);
        assert_eq!(read_session(&path)?.name, "Build and Test Project");
        assert_eq!(
            rename_session(&path, "  release investigation\nignored  ")?,
            "release investigation ignored"
        );
        assert_eq!(read_session(&path)?.name, "release investigation ignored");
        assert!(!set_generated_title(&path, "Late Generated Title")?);
        assert_eq!(read_session(&path)?.name, "release investigation ignored");
        let items = commands(&path)?;
        assert_eq!(command_output(&path, &items[0])?, b"one\r\n");
        assert_eq!(command_output(&path, &items[1])?, b"two\r\n");

        let clip = save_range_as_session(&path, &dir, &items[0], &items[1], "clip")?;
        let clipped_items = commands(&clip)?;
        assert_eq!(clipped_items.len(), 2);
        assert_eq!(clipped_items[0].started_us, 0);
        assert_eq!(command_output(&clip, &clipped_items[0])?, b"one\r\n");
        assert_eq!(command_output(&clip, &clipped_items[1])?, b"two\r\n");

        let legacy = dir.join("legacy.sqlite3");
        std::fs::copy(&path, &legacy)?;
        Connection::open(&legacy)?.execute_batch(
            "PRAGMA application_id = 0;
             PRAGMA user_version = 0;
             DELETE FROM metadata WHERE key = 'summary';",
        )?;
        assert!(read_session(&legacy)?.summary.is_empty());
        std::fs::write(dir.join("commands.txt"), b"not a sqlite session")?;
        let sessions = list_sessions(&dir)?;
        assert!(sessions.iter().any(|session| session.path == legacy));
        assert_eq!(sessions.len(), 3);

        let empty = create_session(&dir, "empty", "sh")?;
        let empty_conn = open_event_writer(&empty)?;
        append_event(&empty_conn, 1, OUTPUT, b"sh$ ")?;
        add_command(&empty_conn, 2, 2, b"exit")?;
        drop(empty_conn);
        finish(&empty, 3)?;
        assert!(discard_if_empty(&empty)?);
        assert!(!empty.exists());

        let interrupted = create_session(&dir, "interrupted", "sh")?;
        let interrupted_conn = open_event_writer(&interrupted)?;
        add_command(&interrupted_conn, 1, 5, b"cargo build")?;
        append_event(&interrupted_conn, 6, OUTPUT, b"Compiling dependency")?;
        drop(interrupted_conn);
        finish(&interrupted, 7)?;
        assert!(!discard_if_empty(&interrupted)?);
        let interrupted_commands = commands(&interrupted)?;
        assert_eq!(
            command_output(&interrupted, &interrupted_commands[0])?,
            b"Compiling dependency"
        );

        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn deleting_a_session_removes_its_database_and_sidecars() -> Result<()> {
        let dir =
            std::env::temp_dir().join(format!("asciinematic-delete-test-{}", uuid::Uuid::new_v4()));
        let path = create_session(&dir, "delete-me", "sh")?;
        let wal = dir.join("delete-me-wal");
        let shm = dir.join("delete-me-shm");
        let journal = dir.join("delete-me-journal");
        std::fs::write(&wal, b"stale")?;
        std::fs::write(&shm, b"stale")?;
        std::fs::write(&journal, b"stale")?;

        delete_session(&path)?;
        assert!(!path.exists());
        assert!(!wal.exists());
        assert!(!shm.exists());
        assert!(!journal.exists());
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
}
