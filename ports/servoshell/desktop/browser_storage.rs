/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Persistent browser storage backed by SQLite.
//! Stores bookmarks, browsing history, and browser settings.

use std::path::PathBuf;
use std::sync::LazyLock;

use std::sync::Mutex;

use log::{error, info};
use rusqlite::{Connection, params};

/// Global database connection, lazily initialized.
static DB: LazyLock<Mutex<Connection>> = LazyLock::new(|| {
    let conn = open_database().expect("Failed to open browser database");
    Mutex::new(conn)
});

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

    // Create tables if they don't exist.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS bookmarks (
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

        CREATE INDEX IF NOT EXISTS idx_history_last_visited ON history(last_visited DESC);
        CREATE INDEX IF NOT EXISTS idx_bookmarks_folder ON bookmarks(folder_id);
        CREATE INDEX IF NOT EXISTS idx_downloads_created ON downloads(created_at DESC);",
    )?;

    Ok(conn)
}

// ── Bookmark types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Bookmark {
    pub id: i64,
    pub url: String,
    pub title: String,
    pub created_at: String,
}

// ── Bookmark operations ─────────────────────────────────────────────────────

pub fn add_bookmark(url: &str, title: &str) -> bool {
    let db = DB.lock().expect("Database lock poisoned");
    match db.execute(
        "INSERT OR REPLACE INTO bookmarks (url, title) VALUES (?1, ?2)",
        params![url, title],
    ) {
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

pub fn remove_bookmark(url: &str) -> bool {
    let db = DB.lock().expect("Database lock poisoned");
    match db.execute("DELETE FROM bookmarks WHERE url = ?1", params![url]) {
        Ok(n) => n > 0,
        Err(e) => {
            error!("Failed to remove bookmark: {}", e);
            false
        },
    }
}

pub fn is_bookmarked(url: &str) -> bool {
    let db = DB.lock().expect("Database lock poisoned");
    db.query_row(
        "SELECT COUNT(*) FROM bookmarks WHERE url = ?1",
        params![url],
        |row| row.get::<_, i64>(0),
    )
    .unwrap_or(0) > 0
}

pub fn get_all_bookmarks() -> Vec<Bookmark> {
    let db = DB.lock().expect("Database lock poisoned");
    let mut stmt = match db.prepare(
        "SELECT id, url, title, created_at FROM bookmarks ORDER BY position, created_at DESC",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    match stmt.query_map([], |row| {
        Ok(Bookmark {
            id: row.get(0)?,
            url: row.get(1)?,
            title: row.get(2)?,
            created_at: row.get(3)?,
        })
    }) {
        Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
        Err(_) => Vec::new(),
    }
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

pub fn record_visit(url: &str, title: &str) {
    let db = DB.lock().expect("Database lock poisoned");
    if let Err(e) = db.execute(
        "INSERT INTO history (url, title) VALUES (?1, ?2)
         ON CONFLICT(url) DO UPDATE SET
            title = ?2,
            visit_count = visit_count + 1,
            last_visited = datetime('now')",
        params![url, title],
    ) {
        error!("Failed to record history: {}", e);
    }
}

pub fn search_history(query: &str, limit: usize) -> Vec<HistoryEntry> {
    let db = DB.lock().expect("Database lock poisoned");
    let pattern = format!("%{}%", query);
    let mut stmt = match db.prepare(
        "SELECT id, url, title, visit_count, last_visited FROM history
         WHERE url LIKE ?1 OR title LIKE ?1
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

pub fn get_recent_history(limit: usize) -> Vec<HistoryEntry> {
    let db = DB.lock().expect("Database lock poisoned");
    let mut stmt = match db.prepare(
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

pub fn clear_all_history() {
    let db = DB.lock().expect("Database lock poisoned");
    if let Err(e) = db.execute("DELETE FROM history", []) {
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

pub fn record_download(url: &str, filename: &str, path: &str, size_bytes: i64) {
    let db = DB.lock().expect("Database lock poisoned");
    if let Err(e) = db.execute(
        "INSERT INTO downloads (url, filename, path, size_bytes, status)
         VALUES (?1, ?2, ?3, ?4, 'complete')",
        params![url, filename, path, size_bytes],
    ) {
        error!("Failed to record download: {}", e);
    }
}

pub fn get_recent_downloads(limit: usize) -> Vec<DownloadRecord> {
    let db = DB.lock().expect("Database lock poisoned");
    let mut stmt = match db.prepare(
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

pub fn clear_all_downloads() {
    let db = DB.lock().expect("Database lock poisoned");
    if let Err(e) = db.execute("DELETE FROM downloads", []) {
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
        "page".to_string()
    } else if trimmed.len() > 200 {
        trimmed[..200].to_string()
    } else {
        trimmed.to_string()
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
    path
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

pub fn get_site_settings(host: &str) -> SiteSettings {
    let db = DB.lock().expect("Database lock poisoned");
    match db.query_row(
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

pub fn save_site_settings(settings: &SiteSettings) {
    let db = DB.lock().expect("Database lock poisoned");
    if let Err(e) = db.execute(
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
    ) {
        error!("Failed to save site settings: {}", e);
    }
}
