use anyhow::{anyhow, bail, Result};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::{
    path::Path,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy)]
pub struct Session {
    pub id: i64,
    pub started_at: i64,
}

#[derive(Clone, Serialize)]
pub struct Caption {
    pub id: u64,
    pub lane: &'static str,
    pub translation: bool,
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub finished: bool,
}

#[derive(Serialize)]
pub struct Entry {
    pub lane: String,
    pub translation: bool,
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

#[derive(Serialize)]
pub struct Saved {
    id: i64,
    started_at: i64,
    count: i64,
    preview: String,
}

pub struct History(Mutex<Connection>);

pub fn timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

impl History {
    pub fn open(directory: &Path) -> Result<Self> {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(directory)?;
        Self::from_connection(Connection::open(directory.join("history.sqlite3"))?)
    }

    fn from_connection(connection: Connection) -> Result<Self> {
        connection.execute_batch(
            "PRAGMA foreign_keys=ON;
             PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS sessions (
                 id INTEGER PRIMARY KEY, started_at INTEGER NOT NULL, title TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS captions (
                 id INTEGER PRIMARY KEY, session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                 lane TEXT NOT NULL, translation INTEGER NOT NULL, text TEXT NOT NULL,
                 start_ms INTEGER NOT NULL, end_ms INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS captions_session ON captions(session_id, start_ms);",
        )?;
        Ok(Self(Mutex::new(connection)))
    }

    fn connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.0
            .lock()
            .map_err(|_| anyhow!("Không thể truy cập lịch sử phiên."))
    }

    pub fn create(&self, title: &str) -> Result<Session> {
        let started_at = timestamp();
        let db = self.connection()?;
        db.execute(
            "INSERT INTO sessions(started_at,title) VALUES (?,?)",
            params![started_at, title],
        )?;
        Ok(Session {
            id: db.last_insert_rowid(),
            started_at,
        })
    }

    pub fn append(&self, session: Session, caption: &Caption) -> Result<()> {
        self.connection()?.execute(
            "INSERT INTO captions(session_id,lane,translation,text,start_ms,end_ms) VALUES (?,?,?,?,?,?)",
            params![session.id, caption.lane, caption.translation, caption.text, caption.start_ms, caption.end_ms],
        )?;
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<Saved>> {
        let db = self.connection()?;
        let mut query = db.prepare(
            "SELECT s.id, s.started_at, count(c.id),
                    COALESCE((SELECT substr(text, 1, 100) FROM captions WHERE session_id=s.id ORDER BY id LIMIT 1),s.title)
             FROM sessions s JOIN captions c ON c.session_id=s.id
             GROUP BY s.id ORDER BY s.id DESC LIMIT 100",
        )?;
        let rows = query.query_map([], |row| {
            Ok(Saved {
                id: row.get(0)?,
                started_at: row.get(1)?,
                count: row.get(2)?,
                preview: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn entries(&self, id: i64) -> Result<Vec<Entry>> {
        let db = self.connection()?;
        let mut query = db.prepare(
            "SELECT lane,translation,text,start_ms,end_ms FROM captions
             WHERE session_id=? ORDER BY start_ms,id",
        )?;
        let rows = query.query_map([id], |row| {
            Ok(Entry {
                lane: row.get(0)?,
                translation: row.get(1)?,
                text: row.get(2)?,
                start_ms: row.get(3)?,
                end_ms: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn delete(&self, id: i64, started_at: i64) -> Result<()> {
        let removed = self.connection()?.execute(
            "DELETE FROM sessions WHERE id=? AND started_at=?",
            params![id, started_at],
        )?;
        if removed == 0 {
            bail!("Phiên đã thay đổi hoặc đã bị xóa. Làm mới lịch sử rồi thử lại.");
        }
        Ok(())
    }
}

pub fn lane_label(lane: &str) -> &'static str {
    match lane {
        "talk" => "Chiều nói",
        "assistant" => "Trợ lý",
        _ => "Chiều nghe",
    }
}

pub fn render(entries: &[Entry], format: &str) -> Result<String> {
    match format {
        "txt" | "md" => {
            let mut text = if format == "md" {
                String::from("# Voxa transcript\n\n")
            } else {
                String::new()
            };
            for entry in entries {
                let stamp = timecode(entry.start_ms).replace(',', ".");
                let role = if entry.translation {
                    "Bản dịch / phản hồi"
                } else {
                    "Lời gốc"
                };
                text.push_str(&format!(
                    "[{stamp}] {} · {role}\n{}\n\n",
                    lane_label(&entry.lane),
                    entry.text.trim()
                ));
            }
            Ok(text)
        }
        "srt" => {
            let translations: Vec<&Entry> =
                entries.iter().filter(|entry| entry.translation).collect();
            let mut text = String::new();
            for (index, entry) in translations.into_iter().enumerate() {
                let start = timecode(entry.start_ms);
                let end = timecode(entry.end_ms.max(entry.start_ms + 500));
                let caption = entry.text.split_whitespace().collect::<Vec<_>>().join(" ");
                text.push_str(&format!("{}\n{start} --> {end}\n{caption}\n\n", index + 1));
            }
            Ok(text)
        }
        _ => Err(anyhow!("Định dạng xuất chưa hỗ trợ.")),
    }
}

fn timecode(ms: i64) -> String {
    let ms = ms.max(0);
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        (ms / 60_000) % 60,
        (ms / 1_000) % 60,
        ms % 1_000
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_row_cannot_delete_a_new_session_with_the_same_id() {
        let history = History::from_connection(Connection::open_in_memory().unwrap()).unwrap();
        let session = history.create("New session").unwrap();
        assert!(history.delete(session.id, session.started_at - 1).is_err());
        assert!(history.delete(session.id, session.started_at).is_ok());
    }

    #[test]
    fn captions_are_persisted_and_rendered_with_subtitle_timing() {
        let history = History::from_connection(Connection::open_in_memory().unwrap()).unwrap();
        let session = history.create("Test").unwrap();
        history
            .append(
                session,
                &Caption {
                    id: 1,
                    lane: "talk",
                    translation: true,
                    text: "Hello".into(),
                    start_ms: 1_250,
                    end_ms: 2_500,
                    finished: true,
                },
            )
            .unwrap();
        let entries = history.entries(session.id).unwrap();
        assert_eq!(
            render(&entries, "srt").unwrap(),
            "1\n00:00:01,250 --> 00:00:02,500\nHello\n\n"
        );
        history.delete(session.id, session.started_at).unwrap();
        assert!(history.entries(session.id).unwrap().is_empty());
    }
}
