/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Persistent browser storage backed by SQLite.
//! Stores bookmarks, browsing history, and browser settings.

use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use log::{error, info};
use rusqlite::{Connection, params};

/// Global database connection, lazily initialized.
static DB: LazyLock<Mutex<Connection>> = LazyLock::new(|| {
    let conn = open_database().expect("Failed to open browser database");
    Mutex::new(conn)
});

/// Schema SQL shared between production and test databases.
const SCHEMA_SQL: &str = "
    CREATE TABLE IF NOT EXISTS bookmarks (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        url TEXT NOT NULL,
        title TEXT NOT NULL DEFAULT '',
        folder_id INTEGER,
        created_at TEXT NOT NULL DEFAULT (datetime('now')),
        position INTEGER NOT NULL DEFAULT 0,
        UNIQUE(url)
    );

    CREATE TABLE IF NOT EXISTS history (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        url TEXT NOT NULL UNIQUE,
        title TEXT NOT NULL DEFAULT '',
        visit_count INTEGER NOT NULL DEFAULT 1,
        last_visited TEXT NOT NULL DEFAULT (datetime('now'))
    );

    CREATE TABLE IF NOT EXISTS site_settings (
        host TEXT PRIMARY KEY NOT NULL,
        content_blocking INTEGER NOT NULL DEFAULT 1,
        cookie_allow INTEGER NOT NULL DEFAULT 0,
        fingerprint_protection INTEGER NOT NULL DEFAULT 1
    );

    CREATE TABLE IF NOT EXISTS downloads (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        url TEXT NOT NULL,
        filename TEXT NOT NULL,
        path TEXT NOT NULL,
        size_bytes INTEGER NOT NULL DEFAULT 0,
        status TEXT NOT NULL DEFAULT 'complete',
        created_at TEXT NOT NULL DEFAULT (datetime('now'))
    );

    CREATE TABLE IF NOT EXISTS bookmark_folders (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        name TEXT NOT NULL,
        parent_id INTEGER REFERENCES bookmark_folders(id) ON DELETE CASCADE,
        position INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL DEFAULT (datetime('now'))
    );

    CREATE INDEX IF NOT EXISTS idx_history_last_visited ON history(last_visited DESC);
    CREATE INDEX IF NOT EXISTS idx_bookmarks_folder ON bookmarks(folder_id);
    CREATE INDEX IF NOT EXISTS idx_downloads_created ON downloads(created_at DESC);
    CREATE INDEX IF NOT EXISTS idx_folders_parent ON bookmark_folders(parent_id);
";

fn database_path() -> PathBuf {
    let config_dir = dirs::config_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("servo-private");
    std::fs::create_dir_all(&config_dir).ok();
    config_dir.join("browser.db")
}

fn open_database() -> Result<Connection, rusqlite::Error> {
    let path = database_path();
    info!("Opening browser database at {:?}", path);
    let conn = Connection::open(&path)?;

    // Enable WAL mode for better concurrent read performance.
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    conn.execute_batch(SCHEMA_SQL)?;

    Ok(conn)
}

// ── Bookmark types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Bookmark {
    pub id: i64,
    pub url: String,
    pub title: String,
    pub folder_id: Option<i64>,
    pub position: i64,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct BookmarkFolder {
    pub id: i64,
    pub name: String,
    pub parent_id: Option<i64>,
    pub position: i64,
}

// ── Bookmark operations ─────────────────────────────────────────────────────

fn add_bookmark_with_conn(conn: &Connection, url: &str, title: &str) -> Result<usize, rusqlite::Error> {
    conn.execute(
        "INSERT OR REPLACE INTO bookmarks (url, title) VALUES (?1, ?2)",
        params![url, title],
    )
}

pub fn add_bookmark(url: &str, title: &str) -> bool {
    let db = DB.lock().expect("Database lock poisoned");
    match add_bookmark_with_conn(&db, url, title) {
        Ok(_) => {
            info!("Bookmarked: {} ({})", title, url);
            true
        },
        Err(e) => {
            error!("Failed to add bookmark: {}", e);
            false
        },
    }
}

fn remove_bookmark_with_conn(conn: &Connection, url: &str) -> Result<usize, rusqlite::Error> {
    conn.execute("DELETE FROM bookmarks WHERE url = ?1", params![url])
}

pub fn remove_bookmark(url: &str) -> bool {
    let db = DB.lock().expect("Database lock poisoned");
    match remove_bookmark_with_conn(&db, url) {
        Ok(n) => n > 0,
        Err(e) => {
            error!("Failed to remove bookmark: {}", e);
            false
        },
    }
}

fn is_bookmarked_with_conn(conn: &Connection, url: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM bookmarks WHERE url = ?1",
        params![url],
        |row| row.get::<_, i64>(0),
    )
    .unwrap_or(0) > 0
}

pub fn is_bookmarked(url: &str) -> bool {
    let db = DB.lock().expect("Database lock poisoned");
    is_bookmarked_with_conn(&db, url)
}

fn get_all_bookmarks_with_conn(conn: &Connection) -> Vec<Bookmark> {
    let mut stmt = match conn.prepare(
        "SELECT id, url, title, folder_id, position, created_at FROM bookmarks ORDER BY position, created_at DESC",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    match stmt.query_map([], |row| {
        Ok(Bookmark {
            id: row.get(0)?,
            url: row.get(1)?,
            title: row.get(2)?,
            folder_id: row.get(3)?,
            position: row.get(4)?,
            created_at: row.get(5)?,
        })
    }) {
        Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
        Err(_) => Vec::new(),
    }
}

pub fn get_all_bookmarks() -> Vec<Bookmark> {
    let db = DB.lock().expect("Database lock poisoned");
    get_all_bookmarks_with_conn(&db)
}

// ── History types ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub id: i64,
    pub url: String,
    pub title: String,
    pub visit_count: i64,
    pub last_visited: String,
}

// ── History operations ──────────────────────────────────────────────────────

fn record_visit_with_conn(conn: &Connection, url: &str, title: &str) -> Result<usize, rusqlite::Error> {
    conn.execute(
        "INSERT INTO history (url, title) VALUES (?1, ?2)
         ON CONFLICT(url) DO UPDATE SET
            title = ?2,
            visit_count = visit_count + 1,
            last_visited = datetime('now')",
        params![url, title],
    )
}

pub fn record_visit(url: &str, title: &str) {
    let db = DB.lock().expect("Database lock poisoned");
    if let Err(e) = record_visit_with_conn(&db, url, title) {
        error!("Failed to record history: {}", e);
    }
}

fn search_history_with_conn(conn: &Connection, query: &str, limit: usize) -> Vec<HistoryEntry> {
    // Escape LIKE wildcards so that literal '%' and '_' in queries don't match everything.
    let escaped = query.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
    let pattern = format!("%{}%", escaped);
    let mut stmt = match conn.prepare(
        "SELECT id, url, title, visit_count, last_visited FROM history
         WHERE url LIKE ?1 ESCAPE '\\' OR title LIKE ?1 ESCAPE '\\'
         ORDER BY last_visited DESC LIMIT ?2",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    match stmt.query_map(params![pattern, limit as i64], |row| {
        Ok(HistoryEntry {
            id: row.get(0)?,
            url: row.get(1)?,
            title: row.get(2)?,
            visit_count: row.get(3)?,
            last_visited: row.get(4)?,
        })
    }) {
        Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
        Err(_) => Vec::new(),
    }
}

pub fn search_history(query: &str, limit: usize) -> Vec<HistoryEntry> {
    let db = DB.lock().expect("Database lock poisoned");
    search_history_with_conn(&db, query, limit)
}

fn get_recent_history_with_conn(conn: &Connection, limit: usize) -> Vec<HistoryEntry> {
    let mut stmt = match conn.prepare(
        "SELECT id, url, title, visit_count, last_visited FROM history
         ORDER BY last_visited DESC LIMIT ?1",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    match stmt.query_map(params![limit as i64], |row| {
        Ok(HistoryEntry {
            id: row.get(0)?,
            url: row.get(1)?,
            title: row.get(2)?,
            visit_count: row.get(3)?,
            last_visited: row.get(4)?,
        })
    }) {
        Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
        Err(_) => Vec::new(),
    }
}

pub fn get_recent_history(limit: usize) -> Vec<HistoryEntry> {
    let db = DB.lock().expect("Database lock poisoned");
    get_recent_history_with_conn(&db, limit)
}

fn clear_all_history_with_conn(conn: &Connection) -> Result<usize, rusqlite::Error> {
    conn.execute("DELETE FROM history", [])
}

pub fn clear_all_history() {
    let db = DB.lock().expect("Database lock poisoned");
    if let Err(e) = clear_all_history_with_conn(&db) {
        error!("Failed to clear history: {}", e);
    }
}

// ── Download types ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DownloadRecord {
    pub id: i64,
    pub url: String,
    pub filename: String,
    pub path: String,
    pub size_bytes: i64,
    pub status: String,
    pub created_at: String,
}

// ── Download operations ────────────────────────────────────────────────────

fn record_download_with_conn(
    conn: &Connection,
    url: &str,
    filename: &str,
    path: &str,
    size_bytes: i64,
) -> Result<usize, rusqlite::Error> {
    conn.execute(
        "INSERT INTO downloads (url, filename, path, size_bytes, status)
         VALUES (?1, ?2, ?3, ?4, 'complete')",
        params![url, filename, path, size_bytes],
    )
}

pub fn record_download(url: &str, filename: &str, path: &str, size_bytes: i64) {
    let db = DB.lock().expect("Database lock poisoned");
    if let Err(e) = record_download_with_conn(&db, url, filename, path, size_bytes) {
        error!("Failed to record download: {}", e);
    }
}

fn get_recent_downloads_with_conn(conn: &Connection, limit: usize) -> Vec<DownloadRecord> {
    let mut stmt = match conn.prepare(
        "SELECT id, url, filename, path, size_bytes, status, created_at
         FROM downloads ORDER BY created_at DESC LIMIT ?1",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    match stmt.query_map(params![limit as i64], |row| {
        Ok(DownloadRecord {
            id: row.get(0)?,
            url: row.get(1)?,
            filename: row.get(2)?,
            path: row.get(3)?,
            size_bytes: row.get(4)?,
            status: row.get(5)?,
            created_at: row.get(6)?,
        })
    }) {
        Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
        Err(_) => Vec::new(),
    }
}

pub fn get_recent_downloads(limit: usize) -> Vec<DownloadRecord> {
    let db = DB.lock().expect("Database lock poisoned");
    get_recent_downloads_with_conn(&db, limit)
}

fn clear_all_downloads_with_conn(conn: &Connection) -> Result<usize, rusqlite::Error> {
    conn.execute("DELETE FROM downloads", [])
}

pub fn clear_all_downloads() {
    let db = DB.lock().expect("Database lock poisoned");
    if let Err(e) = clear_all_downloads_with_conn(&db) {
        error!("Failed to clear downloads: {}", e);
    }
}

/// Save page HTML to the user's Downloads folder and record the download.
/// Called from the evaluate_javascript callback.
pub fn save_page_to_downloads(url: &str, title: &str, html: &str) {
    let downloads_dir = dirs::download_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("Downloads")
        });
    std::fs::create_dir_all(&downloads_dir).ok();

    let sanitized = sanitize_filename(title);
    let filename = format!("{}.html", sanitized);
    let path = downloads_dir.join(&filename);

    // Avoid overwriting — append (1), (2), etc.
    let final_path = deduplicate_path(path);
    let final_filename = final_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or(filename);

    match std::fs::write(&final_path, html) {
        Ok(()) => {
            let size = html.len() as i64;
            info!(
                "Saved page to {:?} ({} bytes)",
                final_path, size
            );
            record_download(url, &final_filename, &final_path.to_string_lossy(), size);
        },
        Err(e) => {
            error!("Failed to save page: {}", e);
        },
    }
}

fn sanitize_filename(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = sanitized.trim().trim_matches('.');
    if trimmed.is_empty() {
        return "page".to_string();
    }
    // Block Windows reserved device names (CON, PRN, AUX, NUL, COM1-9, LPT1-9).
    let stem = trimmed.split('.').next().unwrap_or("");
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL",
        "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
        "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let result = if RESERVED.iter().any(|r| stem.eq_ignore_ascii_case(r)) {
        format!("_{}", trimmed)
    } else {
        trimmed.to_string()
    };
    if result.len() > 200 {
        // Truncate at a valid UTF-8 char boundary to avoid panics on multi-byte titles.
        let mut end = 200;
        while !result.is_char_boundary(end) {
            end -= 1;
        }
        result[..end].to_string()
    } else {
        result
    }
}

fn deduplicate_path(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_default();
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));

    for i in 1..1000 {
        let candidate = parent.join(format!("{} ({}).{}", stem, i, ext));
        if !candidate.exists() {
            return candidate;
        }
    }
    // All 1000 numbered slots taken — use a timestamp to guarantee uniqueness
    // instead of silently overwriting the original file.
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    parent.join(format!("{}_{}.{}", stem, ts, ext))
}

// ── Bookmark folder operations ─────────────────────────────────────────────

fn create_folder_with_conn(conn: &Connection, name: &str, parent_id: Option<i64>) -> Result<i64, rusqlite::Error> {
    conn.execute(
        "INSERT INTO bookmark_folders (name, parent_id) VALUES (?1, ?2)",
        params![name, parent_id],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn create_folder(name: &str, parent_id: Option<i64>) -> Option<i64> {
    let db = DB.lock().expect("Database lock poisoned");
    match create_folder_with_conn(&db, name, parent_id) {
        Ok(id) => Some(id),
        Err(e) => {
            error!("Failed to create folder: {}", e);
            None
        },
    }
}

fn rename_folder_with_conn(conn: &Connection, id: i64, name: &str) -> Result<usize, rusqlite::Error> {
    conn.execute(
        "UPDATE bookmark_folders SET name = ?1 WHERE id = ?2",
        params![name, id],
    )
}

pub fn rename_folder(id: i64, name: &str) {
    let db = DB.lock().expect("Database lock poisoned");
    if let Err(e) = rename_folder_with_conn(&db, id, name) {
        error!("Failed to rename folder: {}", e);
    }
}

fn delete_folder_with_conn(conn: &Connection, id: i64) -> Result<usize, rusqlite::Error> {
    // Move bookmarks in this folder to root (no folder).
    conn.execute(
        "UPDATE bookmarks SET folder_id = NULL WHERE folder_id = ?1",
        params![id],
    )?;
    conn.execute("DELETE FROM bookmark_folders WHERE id = ?1", params![id])
}

pub fn delete_folder(id: i64) {
    let db = DB.lock().expect("Database lock poisoned");
    if let Err(e) = delete_folder_with_conn(&db, id) {
        error!("Failed to delete folder: {}", e);
    }
}

fn get_all_folders_with_conn(conn: &Connection) -> Vec<BookmarkFolder> {
    let mut stmt = match conn.prepare(
        "SELECT id, name, parent_id, position FROM bookmark_folders ORDER BY position, name",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    match stmt.query_map([], |row| {
        Ok(BookmarkFolder {
            id: row.get(0)?,
            name: row.get(1)?,
            parent_id: row.get(2)?,
            position: row.get(3)?,
        })
    }) {
        Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
        Err(_) => Vec::new(),
    }
}

pub fn get_all_folders() -> Vec<BookmarkFolder> {
    let db = DB.lock().expect("Database lock poisoned");
    get_all_folders_with_conn(&db)
}

fn get_bookmarks_in_folder_with_conn(conn: &Connection, folder_id: Option<i64>) -> Vec<Bookmark> {
    let (sql, param_value) = match folder_id {
        Some(fid) => (
            "SELECT id, url, title, folder_id, position, created_at FROM bookmarks WHERE folder_id = ?1 ORDER BY position, created_at DESC",
            Some(fid),
        ),
        None => (
            "SELECT id, url, title, folder_id, position, created_at FROM bookmarks WHERE folder_id IS NULL ORDER BY position, created_at DESC",
            None,
        ),
    };
    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let map_row = |row: &rusqlite::Row| -> rusqlite::Result<Bookmark> {
        Ok(Bookmark {
            id: row.get(0)?,
            url: row.get(1)?,
            title: row.get(2)?,
            folder_id: row.get(3)?,
            position: row.get(4)?,
            created_at: row.get(5)?,
        })
    };
    let result = if let Some(fid) = param_value {
        stmt.query_map(params![fid], map_row)
    } else {
        stmt.query_map([], map_row)
    };
    match result {
        Ok(rows) => rows.filter_map(|r| r.ok()).collect::<Vec<Bookmark>>(),
        Err(_) => Vec::new(),
    }
}

pub fn get_bookmarks_in_folder(folder_id: Option<i64>) -> Vec<Bookmark> {
    let db = DB.lock().expect("Database lock poisoned");
    get_bookmarks_in_folder_with_conn(&db, folder_id)
}

fn move_bookmark_to_folder_with_conn(conn: &Connection, bookmark_id: i64, folder_id: Option<i64>) -> Result<usize, rusqlite::Error> {
    conn.execute(
        "UPDATE bookmarks SET folder_id = ?1 WHERE id = ?2",
        params![folder_id, bookmark_id],
    )
}

pub fn move_bookmark_to_folder(bookmark_id: i64, folder_id: Option<i64>) {
    let db = DB.lock().expect("Database lock poisoned");
    if let Err(e) = move_bookmark_to_folder_with_conn(&db, bookmark_id, folder_id) {
        error!("Failed to move bookmark: {}", e);
    }
}

fn add_bookmark_to_folder_with_conn(conn: &Connection, url: &str, title: &str, folder_id: Option<i64>) -> Result<usize, rusqlite::Error> {
    conn.execute(
        "INSERT OR REPLACE INTO bookmarks (url, title, folder_id) VALUES (?1, ?2, ?3)",
        params![url, title, folder_id],
    )
}

pub fn add_bookmark_to_folder(url: &str, title: &str, folder_id: Option<i64>) -> bool {
    let db = DB.lock().expect("Database lock poisoned");
    match add_bookmark_to_folder_with_conn(&db, url, title, folder_id) {
        Ok(_) => {
            info!("Bookmarked to folder {:?}: {} ({})", folder_id, title, url);
            true
        },
        Err(e) => {
            error!("Failed to add bookmark to folder: {}", e);
            false
        },
    }
}

// ── Site settings types ────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SiteSettings {
    pub host: String,
    pub content_blocking: bool,
    pub cookie_allow: bool,
    pub fingerprint_protection: bool,
}

impl Default for SiteSettings {
    fn default() -> Self {
        Self {
            host: String::new(),
            content_blocking: true,
            cookie_allow: false,
            fingerprint_protection: true,
        }
    }
}

// ── Site settings operations ───────────────────────────────────────────────

fn get_site_settings_with_conn(conn: &Connection, host: &str) -> SiteSettings {
    match conn.query_row(
        "SELECT host, content_blocking, cookie_allow, fingerprint_protection
         FROM site_settings WHERE host = ?1",
        params![host],
        |row| {
            Ok(SiteSettings {
                host: row.get(0)?,
                content_blocking: row.get::<_, i64>(1)? != 0,
                cookie_allow: row.get::<_, i64>(2)? != 0,
                fingerprint_protection: row.get::<_, i64>(3)? != 0,
            })
        },
    ) {
        Ok(settings) => settings,
        Err(_) => SiteSettings {
            host: host.to_string(),
            ..SiteSettings::default()
        },
    }
}

pub fn get_site_settings(host: &str) -> SiteSettings {
    let db = DB.lock().expect("Database lock poisoned");
    get_site_settings_with_conn(&db, host)
}

fn save_site_settings_with_conn(conn: &Connection, settings: &SiteSettings) -> Result<usize, rusqlite::Error> {
    conn.execute(
        "INSERT INTO site_settings (host, content_blocking, cookie_allow, fingerprint_protection)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(host) DO UPDATE SET
            content_blocking = ?2,
            cookie_allow = ?3,
            fingerprint_protection = ?4",
        params![
            settings.host,
            settings.content_blocking as i64,
            settings.cookie_allow as i64,
            settings.fingerprint_protection as i64,
        ],
    )
}

pub fn save_site_settings(settings: &SiteSettings) {
    let db = DB.lock().expect("Database lock poisoned");
    if let Err(e) = save_site_settings_with_conn(&db, settings) {
        error!("Failed to save site settings: {}", e);
    }
}

// ── Bulk clear operations (for Clear Browsing Data dialog) ─────────────────

fn clear_history_since_hours_with_conn(conn: &Connection, hours: u64) -> Result<usize, rusqlite::Error> {
    if hours == 0 {
        return conn.execute("DELETE FROM history", []);
    }
    conn.execute(
        "DELETE FROM history WHERE last_visited >= datetime('now', ?1)",
        params![format!("-{} hours", hours)],
    )
}

pub fn clear_history_since_hours(hours: u64) {
    let db = DB.lock().expect("Database lock poisoned");
    if let Err(e) = clear_history_since_hours_with_conn(&db, hours) {
        error!("Failed to clear history: {}", e);
    }
}

fn clear_downloads_since_hours_with_conn(conn: &Connection, hours: u64) -> Result<usize, rusqlite::Error> {
    if hours == 0 {
        return conn.execute("DELETE FROM downloads", []);
    }
    conn.execute(
        "DELETE FROM downloads WHERE created_at >= datetime('now', ?1)",
        params![format!("-{} hours", hours)],
    )
}

pub fn clear_downloads_since_hours(hours: u64) {
    let db = DB.lock().expect("Database lock poisoned");
    if let Err(e) = clear_downloads_since_hours_with_conn(&db, hours) {
        error!("Failed to clear downloads: {}", e);
    }
}

fn clear_all_bookmarks_with_conn(conn: &Connection) -> Result<usize, rusqlite::Error> {
    conn.execute("DELETE FROM bookmarks", [])
}

pub fn clear_all_bookmarks() {
    let db = DB.lock().expect("Database lock poisoned");
    if let Err(e) = clear_all_bookmarks_with_conn(&db) {
        error!("Failed to clear bookmarks: {}", e);
    }
}

fn clear_all_site_settings_with_conn(conn: &Connection) -> Result<usize, rusqlite::Error> {
    conn.execute("DELETE FROM site_settings", [])
}

pub fn clear_all_site_settings() {
    let db = DB.lock().expect("Database lock poisoned");
    if let Err(e) = clear_all_site_settings_with_conn(&db) {
        error!("Failed to clear site settings: {}", e);
    }
}

// ── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;

    /// Create an in-memory database with the same schema as production.
    /// Each test gets its own isolated connection -- no shared state, no file I/O.
    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().expect("Failed to open in-memory database");
        conn.execute_batch(SCHEMA_SQL).expect("Failed to create test schema");
        conn
    }

    // ── Test 10: Bookmarks CRUD ────────────────────────────────────────────

    #[test]
    fn test_bookmark_add_and_query() {
        let db = test_db();
        assert!(!is_bookmarked_with_conn(&db, "https://example.com"));

        assert!(add_bookmark_with_conn(&db, "https://example.com", "Example").is_ok());
        assert!(is_bookmarked_with_conn(&db, "https://example.com"));

        let bookmarks = get_all_bookmarks_with_conn(&db);
        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks[0].url, "https://example.com");
        assert_eq!(bookmarks[0].title, "Example");
    }

    #[test]
    fn test_bookmark_remove() {
        let db = test_db();
        add_bookmark_with_conn(&db, "https://example.com", "Example").unwrap();
        assert!(is_bookmarked_with_conn(&db, "https://example.com"));

        let removed = remove_bookmark_with_conn(&db, "https://example.com").unwrap();
        assert!(removed > 0);
        assert!(!is_bookmarked_with_conn(&db, "https://example.com"));
        assert_eq!(get_all_bookmarks_with_conn(&db).len(), 0);
    }

    #[test]
    fn test_bookmark_remove_nonexistent() {
        let db = test_db();
        let removed = remove_bookmark_with_conn(&db, "https://nonexistent.com").unwrap();
        assert_eq!(removed, 0);
    }

    #[test]
    fn test_bookmark_upsert_replaces_title() {
        let db = test_db();
        add_bookmark_with_conn(&db, "https://example.com", "Old Title").unwrap();
        add_bookmark_with_conn(&db, "https://example.com", "New Title").unwrap();

        let bookmarks = get_all_bookmarks_with_conn(&db);
        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks[0].title, "New Title");
    }

    #[test]
    fn test_bookmark_multiple_entries() {
        let db = test_db();
        add_bookmark_with_conn(&db, "https://a.com", "A").unwrap();
        add_bookmark_with_conn(&db, "https://b.com", "B").unwrap();
        add_bookmark_with_conn(&db, "https://c.com", "C").unwrap();

        assert_eq!(get_all_bookmarks_with_conn(&db).len(), 3);
        assert!(is_bookmarked_with_conn(&db, "https://b.com"));
        assert!(!is_bookmarked_with_conn(&db, "https://d.com"));
    }

    // ── Test 11: History CRUD ──────────────────────────────────────────────

    #[test]
    fn test_history_record_and_retrieve() {
        let db = test_db();
        record_visit_with_conn(&db, "https://example.com", "Example").unwrap();

        let history = get_recent_history_with_conn(&db, 10);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].url, "https://example.com");
        assert_eq!(history[0].title, "Example");
        assert_eq!(history[0].visit_count, 1);
    }

    #[test]
    fn test_history_visit_count_increments() {
        let db = test_db();
        record_visit_with_conn(&db, "https://example.com", "Example").unwrap();
        record_visit_with_conn(&db, "https://example.com", "Example Updated").unwrap();

        let history = get_recent_history_with_conn(&db, 10);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].visit_count, 2);
        assert_eq!(history[0].title, "Example Updated");
    }

    #[test]
    fn test_history_search() {
        let db = test_db();
        record_visit_with_conn(&db, "https://rust-lang.org", "Rust Programming").unwrap();
        record_visit_with_conn(&db, "https://servo.org", "Servo Browser").unwrap();
        record_visit_with_conn(&db, "https://example.com", "Example").unwrap();

        let results = search_history_with_conn(&db, "rust", 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://rust-lang.org");

        let results = search_history_with_conn(&db, "servo", 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://servo.org");

        // Search by title
        let results = search_history_with_conn(&db, "Browser", 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Servo Browser");
    }

    #[test]
    fn test_history_search_empty_results() {
        let db = test_db();
        record_visit_with_conn(&db, "https://example.com", "Example").unwrap();

        let results = search_history_with_conn(&db, "nonexistent", 10);
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_history_clear() {
        let db = test_db();
        record_visit_with_conn(&db, "https://a.com", "A").unwrap();
        record_visit_with_conn(&db, "https://b.com", "B").unwrap();
        assert_eq!(get_recent_history_with_conn(&db, 10).len(), 2);

        clear_all_history_with_conn(&db).unwrap();
        assert_eq!(get_recent_history_with_conn(&db, 10).len(), 0);
    }

    #[test]
    fn test_history_limit() {
        let db = test_db();
        for i in 0..20 {
            record_visit_with_conn(&db, &format!("https://site{}.com", i), &format!("Site {}", i)).unwrap();
        }

        let history = get_recent_history_with_conn(&db, 5);
        assert_eq!(history.len(), 5);
    }

    // ── Test 12: Downloads CRUD ────────────────────────────────────────────

    #[test]
    fn test_download_record_and_retrieve() {
        let db = test_db();
        record_download_with_conn(&db, "https://example.com/file.zip", "file.zip", "/tmp/file.zip", 1024).unwrap();

        let downloads = get_recent_downloads_with_conn(&db, 10);
        assert_eq!(downloads.len(), 1);
        assert_eq!(downloads[0].url, "https://example.com/file.zip");
        assert_eq!(downloads[0].filename, "file.zip");
        assert_eq!(downloads[0].path, "/tmp/file.zip");
        assert_eq!(downloads[0].size_bytes, 1024);
        assert_eq!(downloads[0].status, "complete");
    }

    #[test]
    fn test_download_clear() {
        let db = test_db();
        record_download_with_conn(&db, "https://a.com/a.zip", "a.zip", "/tmp/a.zip", 100).unwrap();
        record_download_with_conn(&db, "https://b.com/b.zip", "b.zip", "/tmp/b.zip", 200).unwrap();
        assert_eq!(get_recent_downloads_with_conn(&db, 10).len(), 2);

        clear_all_downloads_with_conn(&db).unwrap();
        assert_eq!(get_recent_downloads_with_conn(&db, 10).len(), 0);
    }

    #[test]
    fn test_download_limit() {
        let db = test_db();
        for i in 0..15 {
            record_download_with_conn(
                &db,
                &format!("https://example.com/{}.zip", i),
                &format!("{}.zip", i),
                &format!("/tmp/{}.zip", i),
                i * 100,
            ).unwrap();
        }

        let downloads = get_recent_downloads_with_conn(&db, 5);
        assert_eq!(downloads.len(), 5);
    }

    // ── Test 12b: Filename sanitization ────────────────────────────────────

    #[test]
    fn test_sanitize_filename_special_chars() {
        assert_eq!(sanitize_filename("hello/world"), "hello_world");
        assert_eq!(sanitize_filename("file:name"), "file_name");
        assert_eq!(sanitize_filename("a*b?c\"d<e>f|g"), "a_b_c_d_e_f_g");
        assert_eq!(sanitize_filename("back\\slash"), "back_slash");
    }

    #[test]
    fn test_sanitize_filename_control_chars() {
        assert_eq!(sanitize_filename("hello\x00world"), "hello_world");
        assert_eq!(sanitize_filename("tab\there"), "tab_here");
    }

    #[test]
    fn test_sanitize_filename_empty_and_dots() {
        assert_eq!(sanitize_filename(""), "page");
        assert_eq!(sanitize_filename("..."), "page");
        assert_eq!(sanitize_filename("   "), "page");
        assert_eq!(sanitize_filename(". . ."), ". .");
    }

    #[test]
    fn test_sanitize_filename_long_name() {
        let long_name = "a".repeat(300);
        let sanitized = sanitize_filename(&long_name);
        assert_eq!(sanitized.len(), 200);
    }

    #[test]
    fn test_sanitize_filename_normal() {
        assert_eq!(sanitize_filename("My Document"), "My Document");
        assert_eq!(sanitize_filename("report-2026.pdf"), "report-2026.pdf");
    }

    #[test]
    fn test_sanitize_filename_windows_reserved() {
        assert_eq!(sanitize_filename("CON"), "_CON");
        assert_eq!(sanitize_filename("PRN"), "_PRN");
        assert_eq!(sanitize_filename("AUX"), "_AUX");
        assert_eq!(sanitize_filename("NUL"), "_NUL");
        assert_eq!(sanitize_filename("COM1"), "_COM1");
        assert_eq!(sanitize_filename("LPT1"), "_LPT1");
        assert_eq!(sanitize_filename("con"), "_con"); // case-insensitive
        assert_eq!(sanitize_filename("normal"), "normal"); // not reserved
    }

    #[test]
    fn test_sanitize_filename_multibyte_truncation() {
        // 100 Chinese characters = 300 UTF-8 bytes. Truncation at byte 200
        // must not panic by landing mid-character.
        let cjk = "\u{4e2d}".repeat(100); // 300 bytes
        let sanitized = sanitize_filename(&cjk);
        assert!(sanitized.len() <= 200);
        assert!(sanitized.is_char_boundary(sanitized.len()));
        // Should be 66 chars (66 * 3 = 198 bytes, the last boundary before 200)
        assert_eq!(sanitized.chars().count(), 66);
    }

    // ── Test 12c: Path deduplication ───────────────────────────────────────

    #[test]
    fn test_deduplicate_path_no_conflict() {
        use std::path::PathBuf;
        let path = PathBuf::from("/nonexistent/dir/test.html");
        assert_eq!(deduplicate_path(path.clone()), path);
    }

    #[test]
    fn test_deduplicate_path_with_conflict() {
        use std::path::PathBuf;
        let dir = std::env::temp_dir().join("servo_test_dedup");
        std::fs::create_dir_all(&dir).ok();
        let original = dir.join("test.html");
        std::fs::write(&original, "content").ok();

        let result = deduplicate_path(original.clone());
        assert_eq!(result, dir.join("test (1).html"));

        // Create the (1) variant too
        std::fs::write(&result, "content").ok();
        let result2 = deduplicate_path(original.clone());
        assert_eq!(result2, dir.join("test (2).html"));

        // Cleanup
        std::fs::remove_dir_all(&dir).ok();
    }

    // ── Test 13: Site Settings CRUD ────────────────────────────────────────

    #[test]
    fn test_site_settings_defaults_for_unknown_host() {
        let db = test_db();
        let settings = get_site_settings_with_conn(&db, "unknown.com");

        assert_eq!(settings.host, "unknown.com");
        assert!(settings.content_blocking);
        assert!(!settings.cookie_allow);
        assert!(settings.fingerprint_protection);
    }

    #[test]
    fn test_site_settings_save_and_retrieve() {
        let db = test_db();
        let settings = SiteSettings {
            host: "example.com".to_string(),
            content_blocking: false,
            cookie_allow: true,
            fingerprint_protection: false,
        };
        save_site_settings_with_conn(&db, &settings).unwrap();

        let loaded = get_site_settings_with_conn(&db, "example.com");
        assert_eq!(loaded.host, "example.com");
        assert!(!loaded.content_blocking);
        assert!(loaded.cookie_allow);
        assert!(!loaded.fingerprint_protection);
    }

    #[test]
    fn test_site_settings_upsert() {
        let db = test_db();
        let settings = SiteSettings {
            host: "example.com".to_string(),
            content_blocking: true,
            cookie_allow: false,
            fingerprint_protection: true,
        };
        save_site_settings_with_conn(&db, &settings).unwrap();

        let updated = SiteSettings {
            host: "example.com".to_string(),
            content_blocking: false,
            cookie_allow: true,
            fingerprint_protection: false,
        };
        save_site_settings_with_conn(&db, &updated).unwrap();

        let loaded = get_site_settings_with_conn(&db, "example.com");
        assert!(!loaded.content_blocking);
        assert!(loaded.cookie_allow);
        assert!(!loaded.fingerprint_protection);
    }

    #[test]
    fn test_site_settings_multiple_hosts() {
        let db = test_db();
        save_site_settings_with_conn(&db, &SiteSettings {
            host: "a.com".to_string(),
            content_blocking: false,
            cookie_allow: true,
            fingerprint_protection: true,
        }).unwrap();
        save_site_settings_with_conn(&db, &SiteSettings {
            host: "b.com".to_string(),
            content_blocking: true,
            cookie_allow: false,
            fingerprint_protection: false,
        }).unwrap();

        let a = get_site_settings_with_conn(&db, "a.com");
        let b = get_site_settings_with_conn(&db, "b.com");

        assert!(!a.content_blocking);
        assert!(a.cookie_allow);
        assert!(b.content_blocking);
        assert!(!b.cookie_allow);
    }
}
