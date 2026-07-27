use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use directories::BaseDirs;
use rusqlite::{Connection, params};

pub const OUTPUT: i64 = 0;
pub const INPUT: i64 = 1;

#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub name: String,
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

pub fn create_session(dir: &Path, id: &str, name: &str, program: &str) -> Result<PathBuf> {
    fs::create_dir_all(dir)
        .with_context(|| format!("failed to create session directory {}", dir.display()))?;
    let path = dir.join(format!("{id}.sqlite3"));
    let conn = Connection::open(&path)?;
    conn.execute_batch(
        "
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
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
    for (key, value) in [
        ("format_version", "1"),
        ("id", id),
        ("name", name),
        ("program", program),
        ("started_at", &started_at),
        ("duration_us", "0"),
    ] {
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
    conn.execute(
        "UPDATE commands SET ended_us = ?1 WHERE ended_us IS NULL",
        [duration_us],
    )?;
    conn.execute(
        "UPDATE metadata SET value = ?1 WHERE key = 'duration_us'",
        [duration_us.to_string()],
    )?;
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

pub fn list_sessions(dir: &Path) -> Result<Vec<Session>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut sessions = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if !matches!(
            path.extension().and_then(|v| v.to_str()),
            Some("sqlite3" | "sqlite")
        ) {
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
    let meta = |key: &str| -> Result<String> {
        conn.query_row("SELECT value FROM metadata WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .with_context(|| format!("missing {key} metadata"))
    };
    let command_count = conn.query_row("SELECT count(*) FROM commands", [], |row| row.get(0))?;
    Ok(Session {
        id: meta("id")?,
        name: meta("name")?,
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
    name: &str,
) -> Result<PathBuf> {
    let source_conn = Connection::open(source)?;
    let program: String = source_conn.query_row(
        "SELECT value FROM metadata WHERE key = 'program'",
        [],
        |row| row.get(0),
    )?;
    let destination = create_session(dir, id, name, &program)?;
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
        let path = create_session(&dir, "test", "test", "sh")?;
        let conn = open_event_writer(&path)?;
        add_command(&conn, 1, 10, b"echo one")?;
        append_event(&conn, 11, OUTPUT, b"one\r\n")?;
        add_command(&conn, 2, 20, b"echo two")?;
        append_event(&conn, 21, OUTPUT, b"two\r\n")?;
        drop(conn);
        finish(&path, 30)?;
        let items = commands(&path)?;
        assert_eq!(command_output(&path, &items[0])?, b"one\r\n");
        assert_eq!(command_output(&path, &items[1])?, b"two\r\n");

        let clip = save_range_as_session(&path, &dir, &items[0], &items[1], "clip", "clip")?;
        let clipped_items = commands(&clip)?;
        assert_eq!(clipped_items.len(), 2);
        assert_eq!(clipped_items[0].started_us, 0);
        assert_eq!(command_output(&clip, &clipped_items[0])?, b"one\r\n");
        assert_eq!(command_output(&clip, &clipped_items[1])?, b"two\r\n");

        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
}
