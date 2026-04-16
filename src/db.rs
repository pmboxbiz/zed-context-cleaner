use std::path::PathBuf;

use anyhow::Result;
use rusqlite::{Connection, OpenFlags};

use crate::types::{DbThread, ThreadMeta};

/// Returns the default path to threads.db based on the OS
pub fn default_db_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        dirs::data_local_dir().map(|p| p.join("Zed").join("threads").join("threads.db"))
    }
    #[cfg(target_os = "macos")]
    {
        dirs::data_dir().map(|p| p.join("Zed").join("threads").join("threads.db"))
    }
    #[cfg(target_os = "linux")]
    {
        dirs::data_local_dir().map(|p| p.join("zed").join("threads").join("threads.db"))
    }
}

/// Open the SQLite database in read-write mode (no create)
pub fn open_db(path: &std::path::Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    Ok(conn)
}

/// Fast thread type detection by searching for subagent_context marker.
/// No JSON parsing — just byte-level search after zstd decompress.
/// Returns "Subagent" or "Chat".
fn detect_type_fast(data: &[u8], data_type: &str) -> String {
    let bytes = match data_type {
        "zstd" => match zstd::stream::decode_all(data) {
            Ok(d) => d,
            Err(_) => return String::new(),
        },
        "json" => data.to_vec(),
        _ => return String::new(),
    };

    // "subagent_context":null  → Chat (main thread)
    // "subagent_context":{...} → Subagent (spawned by spawn_agent)
    let subagent_key = b"\"subagent_context\"";
    let subagent_null = b"\"subagent_context\":null";
    let has_subagent_key = bytes.windows(subagent_key.len()).any(|w| w == subagent_key);
    let has_subagent_null = bytes
        .windows(subagent_null.len())
        .any(|w| w == subagent_null);

    if has_subagent_key && !has_subagent_null {
        "Subagent".to_string()
    } else {
        "Chat".to_string()
    }
}

/// Load the list of all threads with fast type detection
pub fn load_thread_list(conn: &Connection) -> Result<Vec<ThreadMeta>> {
    let mut stmt = conn.prepare(
        "SELECT id, summary, updated_at, created_at, data_type, length(data), data \
         FROM threads ORDER BY updated_at DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        let data_type: String = row.get::<_, String>(4).unwrap_or_default();
        let data: Vec<u8> = row.get::<_, Vec<u8>>(6).unwrap_or_default();
        let thread_type = detect_type_fast(&data, &data_type);
        Ok(ThreadMeta {
            id: row.get(0)?,
            summary: row.get::<_, String>(1).unwrap_or_default(),
            updated_at: row.get::<_, String>(2).unwrap_or_default(),
            created_at: row.get::<_, String>(3).unwrap_or_default(),
            data_type,
            data_size: row.get::<_, usize>(5).unwrap_or(0),
            thread_type,
        })
    })?;
    let mut list = Vec::new();
    for row in rows {
        list.push(row?);
    }
    Ok(list)
}

/// Decompress data blob based on data_type
fn decompress_data(data: &[u8], data_type: &str) -> Result<String> {
    match data_type {
        "zstd" => {
            // zstd::stream::decode_all accepts impl Read; &[u8] implements Read.
            let decompressed = zstd::stream::decode_all(data)?;
            Ok(String::from_utf8(decompressed)?)
        }
        "json" => Ok(String::from_utf8(data.to_vec())?),
        other => anyhow::bail!("Unknown data_type: {}", other),
    }
}

/// Compress JSON string based on data_type
fn compress_data(json: &str, data_type: &str) -> Result<Vec<u8>> {
    match data_type {
        // zstd::stream::encode_all accepts (impl Read, i32); &[u8] implements Read.
        "zstd" => Ok(zstd::stream::encode_all(json.as_bytes(), 3)?),
        "json" => Ok(json.as_bytes().to_vec()),
        other => anyhow::bail!("Unknown data_type: {}", other),
    }
}

/// Load a full thread by ID. Returns (DbThread, raw_json_string, data_type)
pub fn load_thread(conn: &Connection, id: &str) -> Result<(DbThread, String, String)> {
    let mut stmt = conn.prepare("SELECT data_type, data FROM threads WHERE id = ?")?;
    let (data_type, data): (String, Vec<u8>) =
        stmt.query_row([id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let json_str = decompress_data(&data, &data_type)?;
    let thread: DbThread = serde_json::from_str(&json_str)?;
    Ok((thread, json_str, data_type))
}

/// Save a cleaned thread back to the DB
pub fn save_thread(conn: &Connection, id: &str, thread: &DbThread, data_type: &str) -> Result<()> {
    let json_str = serde_json::to_string_pretty(thread)?;
    let blob = compress_data(&json_str, data_type)?;
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE threads SET data = ?, updated_at = ? WHERE id = ?",
        rusqlite::params![blob, now, id],
    )?;
    Ok(())
}
