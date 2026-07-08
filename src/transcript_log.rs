//! Персистентное хранилище звонков («колов»): транскрипция + метаданные аудио-дорожек.
//!
//! Кол начинается по кнопке в виджете и группирует: строки транскрипции (обоих каналов) и
//! две аудио-дорожки на диске (микрофон + звук созвона, НЕ склеенные). В БД лежат
//! метаданные (название, дата, пути дорожек) и весь текст с таймингом; сам звук — WAV-файлы
//! рядом (`calls/<id>/…`). Вне кола транскрипция всё равно пишется (call_id = NULL).
//!
//! Посмотреть: `health-widget --transcript` (весь текст), `--calls` (список колов с дорожками).
//! БД — `~/.local/share/health-widget/transcripts.db`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{params, Connection};

/// Открытое хранилище одного сеанса виджета.
pub struct TranscriptLog {
    conn: Mutex<Connection>,
    /// Метка старта виджета — группирует сегменты одного запуска.
    session: String,
    /// Активный кол (его id) — им помечаются строки транскрипции. None — вне кола.
    current_call: Mutex<Option<i64>>,
}

impl TranscriptLog {
    /// Открыть/создать БД в data-dir. None — если не открылась.
    pub fn open() -> Option<Self> {
        let path = db_path()?;
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let conn = Connection::open(&path).ok()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS transcript (
                 id      INTEGER PRIMARY KEY,
                 session TEXT NOT NULL,
                 call_id INTEGER,
                 ts      TEXT NOT NULL DEFAULT (datetime('now','localtime')),
                 channel TEXT NOT NULL,
                 text    TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_transcript_ts ON transcript(ts);
             CREATE TABLE IF NOT EXISTS calls (
                 id         INTEGER PRIMARY KEY,
                 name       TEXT NOT NULL,
                 started_at TEXT NOT NULL DEFAULT (datetime('now','localtime')),
                 ended_at   TEXT
             );
             CREATE TABLE IF NOT EXISTS tracks (
                 call_id INTEGER NOT NULL,
                 channel TEXT NOT NULL,
                 path    TEXT NOT NULL,
                 PRIMARY KEY (call_id, channel)
             );",
        )
        .ok()?;
        // Миграция старой БД (transcript без call_id — из прошлой версии).
        let has_call = conn
            .prepare("PRAGMA table_info(transcript)")
            .and_then(|mut s| {
                s.query_map([], |r| r.get::<_, String>(1))
                    .map(|rows| rows.flatten().any(|c| c == "call_id"))
            })
            .unwrap_or(true);
        if !has_call {
            let _ = conn.execute("ALTER TABLE transcript ADD COLUMN call_id INTEGER", []);
        }
        let session: String = conn
            .query_row("SELECT datetime('now','localtime')", [], |r| r.get(0))
            .ok()?;
        Some(Self {
            conn: Mutex::new(conn),
            session,
            current_call: Mutex::new(None),
        })
    }

    /// Дописать финальный сегмент канала (с привязкой к активному колу, если он есть).
    pub fn append(&self, channel: &str, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        // Читаем call_id ДО блокировки conn (не держим два лока разом).
        let call_id = self.current_call.lock().ok().and_then(|g| *g);
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute(
                "INSERT INTO transcript(session, call_id, channel, text) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![self.session, call_id, channel, text],
            );
        }
    }

    /// Начать кол с названием `name` — вернуть его id и сделать активным. None — при ошибке.
    pub fn start_call(&self, name: &str) -> Option<i64> {
        let id = {
            let conn = self.conn.lock().ok()?;
            conn.execute("INSERT INTO calls(name) VALUES (?1)", params![name]).ok()?;
            conn.last_insert_rowid()
        };
        if let Ok(mut g) = self.current_call.lock() {
            *g = Some(id);
        }
        Some(id)
    }

    /// Завершить кол: проставить время окончания и снять активность.
    pub fn end_call(&self, id: i64) {
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute(
                "UPDATE calls SET ended_at = datetime('now','localtime') \
                 WHERE id = ?1 AND ended_at IS NULL",
                params![id],
            );
        }
        if let Ok(mut g) = self.current_call.lock() {
            if *g == Some(id) {
                *g = None;
            }
        }
    }

    /// Зафиксировать путь аудио-дорожки канала для кола.
    pub fn add_track(&self, call_id: i64, channel: &str, path: &str) {
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute(
                "INSERT OR REPLACE INTO tracks(call_id, channel, path) VALUES (?1, ?2, ?3)",
                params![call_id, channel, path],
            );
        }
    }

    /// Выгрузить транскрипцию хронологически (для CLI). `today_only` — только сегодня.
    pub fn dump(today_only: bool) -> Option<String> {
        let conn = Connection::open(db_path()?).ok()?;
        let sql = if today_only {
            "SELECT ts, channel, text FROM transcript \
             WHERE date(ts) = date('now','localtime') ORDER BY id"
        } else {
            "SELECT ts, channel, text FROM transcript ORDER BY id"
        };
        let mut stmt = conn.prepare(sql).ok()?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .ok()?;
        let mut out = String::new();
        for (ts, channel, text) in rows.flatten() {
            out.push_str(&format!("[{ts}] {channel}: {text}\n"));
        }
        Some(out)
    }

    /// Список колов с дорожками и числом строк транскрипции (для CLI).
    pub fn list_calls() -> Option<String> {
        let conn = Connection::open(db_path()?).ok()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, name, started_at, COALESCE(ended_at, '…') FROM calls ORDER BY id",
            )
            .ok()?;
        let calls = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .ok()?;
        let mut out = String::new();
        for (id, name, started, ended) in calls.flatten() {
            let lines: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM transcript WHERE call_id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            out.push_str(&format!(
                "#{id}  «{name}»  {started} → {ended}  ({lines} строк текста)\n"
            ));
            let mut ts = conn
                .prepare("SELECT channel, path FROM tracks WHERE call_id = ?1 ORDER BY channel")
                .ok()?;
            let tracks = ts
                .query_map(params![id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .ok()?;
            for (channel, path) in tracks.flatten() {
                out.push_str(&format!("    {channel}: {path}\n"));
            }
        }
        Some(out)
    }

    /// Экспортировать кол в папку `dest`: обе WAV-дорожки + `transcript.txt` рядом.
    /// Возвращает путь созданной подпапки `<dest>/<id>-<название>/`. Ошибку — строкой.
    pub fn export_call(id: i64, dest: &Path) -> Result<PathBuf, String> {
        let conn =
            Connection::open(db_path().ok_or("нет data-dir")?).map_err(|e| e.to_string())?;
        let (name, started, ended): (String, String, String) = conn
            .query_row(
                "SELECT name, started_at, COALESCE(ended_at, '…') FROM calls WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(|_| format!("кол #{id} не найден"))?;

        let target = dest.join(format!("{id}-{}", sanitize(&name)));
        std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;

        // Копируем дорожки (файл мог быть удалён — тогда отмечаем, но не падаем).
        let mut tstmt = conn
            .prepare("SELECT channel, path FROM tracks WHERE call_id = ?1 ORDER BY channel")
            .map_err(|e| e.to_string())?;
        let tracks: Vec<(String, String)> = tstmt
            .query_map(params![id], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?
            .flatten()
            .collect();
        let mut missing = Vec::new();
        for (_channel, path) in &tracks {
            let src = Path::new(path);
            let fname = src.file_name().unwrap_or_else(|| std::ffi::OsStr::new("track.wav"));
            if std::fs::copy(src, target.join(fname)).is_err() {
                missing.push(path.clone());
            }
        }

        // Транскрипт кола рядом.
        let mut out = format!("Кол #{id}: {name}\n{started} → {ended}\n\n");
        let mut sstmt = conn
            .prepare(
                "SELECT ts, channel, text FROM transcript WHERE call_id = ?1 ORDER BY id",
            )
            .map_err(|e| e.to_string())?;
        let rows = sstmt
            .query_map(params![id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for (ts, channel, text) in rows.flatten() {
            out.push_str(&format!("[{ts}] {channel}: {text}\n"));
        }
        std::fs::write(target.join("transcript.txt"), out).map_err(|e| e.to_string())?;

        if !missing.is_empty() {
            eprintln!("⚠ не найдены файлы дорожек: {}", missing.join(", "));
        }
        Ok(target)
    }
}

/// Привести название кола к безопасному для имени папки виду (пробелы → «_», спецсимволы убираем).
fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .filter(|c| !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .collect();
    let s = s.trim_matches('_');
    if s.is_empty() {
        "без_названия".to_string()
    } else {
        s.to_string()
    }
}

/// Путь к БД транскрипции (аудио-дорожки — рядом, в `calls/<id>/`).
fn db_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("health-widget").join("transcripts.db"))
}

/// Каталог аудио-дорожек кола: `~/.local/share/health-widget/calls/<id>/`.
pub fn call_dir(call_id: i64) -> Option<PathBuf> {
    dirs::data_dir().map(|d| {
        d.join("health-widget")
            .join("calls")
            .join(call_id.to_string())
    })
}
