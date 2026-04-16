mod app;
mod cleaner;
mod db;
mod types;

use std::io::Write;
use std::path::PathBuf;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn setup_logging() {
    let log_path = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("zed-context-cleaner.log")))
        .unwrap_or_else(|| std::path::PathBuf::from("zed-context-cleaner.log"));

    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .expect("Failed to open log file");

    let file = std::sync::Mutex::new(file);

    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Debug)
        .format(move |_buf, record| {
            let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
            let line = format!(
                "[{}] {} [{}:{}] {}\n",
                now,
                record.level(),
                record.file().unwrap_or("unknown"),
                record.line().unwrap_or(0),
                record.args(),
            );
            if let Ok(mut f) = file.lock() {
                let _ = f.write_all(line.as_bytes());
                let _ = f.flush();
            }
            eprint!("{}", line);
            Ok(())
        })
        .init();

    log::info!("=== Zed Context Cleaner started ===");
    log::info!("Log file: {}", log_path.display());
}

fn format_bytes(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn get_db_path() -> Result<PathBuf, String> {
    db::default_db_path()
        .filter(|p| p.exists())
        .ok_or_else(|| "Cannot find threads.db. Is Zed installed?".to_string())
}

fn get_backup_dir() -> Result<PathBuf, String> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("backups")))
        .ok_or_else(|| "Cannot determine backup directory".to_string())
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.chars().nth(8) == Some('-')
        && s.chars().nth(13) == Some('-')
        && s.chars().nth(18) == Some('-')
        && s.chars().nth(23) == Some('-')
        && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

fn print_usage() {
    eprintln!("Zed Context Cleaner v{}", VERSION);
    eprintln!();
    eprintln!("Usage:");
    eprintln!("  zed-context-cleaner                          Launch GUI");
    eprintln!("  zed-context-cleaner list                     List all threads");
    eprintln!("  zed-context-cleaner clean <thread_id>        Backup & clean a thread");
    eprintln!("  zed-context-cleaner clean <title>            Search by title and clean");
    eprintln!("  zed-context-cleaner clean <id_or_title> -n 5 Keep last 5 dialogs (default: 10)");
    eprintln!("  zed-context-cleaner restore <backup_file>    Restore thread from backup JSON");
    eprintln!();
    eprintln!("Examples:");
    eprintln!("  zed-context-cleaner list");
    eprintln!("  zed-context-cleaner clean 77016878-6e31-48e6-8c25-7d607fd60728");
    eprintln!("  zed-context-cleaner clean \"VPN App Icon\"");
    eprintln!("  zed-context-cleaner restore 77016878-6e31-48e6-8c25-7d607fd60728_My_Thread_20260416_190000.json");
}

/// Resolve a thread identifier: if it's a UUID use it directly,
/// otherwise search by title (case-insensitive substring match).
/// If multiple matches, print them and ask user to be more specific.
fn resolve_thread_id(query: &str) -> Result<String, String> {
    if is_uuid(query) {
        return Ok(query.to_string());
    }

    let db_path = get_db_path()?;
    let conn = db::open_db(&db_path).map_err(|e| format!("Failed to open DB: {}", e))?;
    let threads =
        db::load_thread_list(&conn).map_err(|e| format!("Failed to load threads: {}", e))?;

    let query_lower = query.to_lowercase();
    let matches: Vec<&types::ThreadMeta> = threads
        .iter()
        .filter(|t| t.summary.to_lowercase().contains(&query_lower))
        .collect();

    match matches.len() {
        0 => Err(format!("No thread found matching '{}'", query)),
        1 => {
            println!("Found: \"{}\" ({})", matches[0].summary, matches[0].id);
            Ok(matches[0].id.clone())
        }
        n => {
            eprintln!("Multiple threads match '{}' ({} found):", query, n);
            eprintln!();
            for (i, t) in matches.iter().enumerate().take(20) {
                eprintln!(
                    "  {:>2}. {} {:<8} {}",
                    i + 1,
                    t.id,
                    format_bytes(t.data_size),
                    if t.summary.len() > 60 {
                        format!("{}...", t.summary.chars().take(57).collect::<String>())
                    } else {
                        t.summary.clone()
                    }
                );
            }
            if n > 20 {
                eprintln!("  ... and {} more", n - 20);
            }
            eprintln!();
            Err("Be more specific or use the full thread ID.".to_string())
        }
    }
}

fn cmd_list() -> Result<(), String> {
    let db_path = get_db_path()?;
    let conn = db::open_db(&db_path).map_err(|e| format!("Failed to open DB: {}", e))?;
    let threads =
        db::load_thread_list(&conn).map_err(|e| format!("Failed to load threads: {}", e))?;

    println!("{:<40} {:<10} {:<8} {}", "ID", "Type", "Size", "Summary");
    println!("{}", "-".repeat(100));

    for t in &threads {
        let summary: String = if t.summary.len() > 60 {
            t.summary.chars().take(57).collect::<String>() + "..."
        } else if t.summary.is_empty() {
            "(no summary)".to_string()
        } else {
            t.summary.clone()
        };
        println!(
            "{:<40} {:<10} {:<8} {}",
            t.id,
            t.thread_type,
            format_bytes(t.data_size),
            summary,
        );
    }

    println!();
    println!("Total: {} threads", threads.len());
    Ok(())
}

fn cmd_clean(thread_id: &str, keep_last_n: usize) -> Result<(), String> {
    let db_path = get_db_path()?;
    let conn = db::open_db(&db_path).map_err(|e| format!("Failed to open DB: {}", e))?;

    println!("Loading thread {}...", thread_id);
    let (thread, raw_json, data_type) =
        db::load_thread(&conn, thread_id).map_err(|e| format!("Failed to load thread: {}", e))?;

    let old_size = raw_json.len();
    let title = thread.title.as_deref().unwrap_or("untitled");
    println!("  Title:  {}", title);
    println!("  Size:   {} (uncompressed JSON)", format_bytes(old_size));
    println!("  Messages: {}", thread.messages.len());

    // Create backup
    let backup_dir = get_backup_dir()?;
    std::fs::create_dir_all(&backup_dir).map_err(|e| format!("Cannot create backup dir: {}", e))?;

    let summary_safe: String = title
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_' || *c == ' ')
        .take(40)
        .collect::<String>()
        .trim()
        .replace(' ', "_");
    let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let backup_filename = format!("{}_{}_{}.json", thread_id, summary_safe, timestamp);
    let backup_path = backup_dir.join(&backup_filename);

    let pretty_backup = {
        let v: serde_json::Value = serde_json::from_str(&raw_json)
            .map_err(|e| format!("Failed to parse JSON for backup: {}", e))?;
        serde_json::to_string_pretty(&v)
            .map_err(|e| format!("Failed to format JSON: {}", e))?
    };
    std::fs::write(&backup_path, &pretty_backup)
        .map_err(|e| format!("Failed to write backup: {}", e))?;
    println!("  Backup: {}", backup_path.display());

    // Clean
    let config = cleaner::CleanConfig {
        keep_last_n_dialogs: keep_last_n,
        ..Default::default()
    };
    println!("  Cleaning (keep last {} dialogs)...", keep_last_n);
    let cleaned = cleaner::clean_thread(&thread, &config);

    let new_json =
        serde_json::to_string_pretty(&cleaned).map_err(|e| format!("Serialize error: {}", e))?;
    let new_size = new_json.len();

    // Save to DB
    db::save_thread(&conn, thread_id, &cleaned, &data_type)
        .map_err(|e| format!("Failed to save: {}", e))?;

    let saved = old_size.saturating_sub(new_size);
    let pct = if old_size > 0 {
        saved as f64 / old_size as f64 * 100.0
    } else {
        0.0
    };

    println!();
    println!("  Done!");
    println!("  Before: {}", format_bytes(old_size));
    println!("  After:  {}", format_bytes(new_size));
    println!("  Saved:  {} (-{:.1}%)", format_bytes(saved), pct);
    println!();
    println!("You can now open Zed. Context window size will update after the first message.");

    Ok(())
}

fn cmd_restore(backup_file: &str) -> Result<(), String> {
    // Find the backup file вЂ” check as-is, then in backups/ dir
    let backup_path = if std::path::Path::new(backup_file).exists() {
        PathBuf::from(backup_file)
    } else {
        let in_backups = get_backup_dir()?.join(backup_file);
        if in_backups.exists() {
            in_backups
        } else {
            return Err(format!(
                "Backup file not found: '{}'\nAlso checked: backups/{}",
                backup_file, backup_file
            ));
        }
    };

    println!("Reading backup: {}", backup_path.display());
    let json_str = std::fs::read_to_string(&backup_path)
        .map_err(|e| format!("Failed to read backup: {}", e))?;

    // Validate JSON
    let _: serde_json::Value = serde_json::from_str(&json_str)
        .map_err(|e| format!("Backup contains invalid JSON: {}", e))?;

    // Extract thread_id from filename: {thread_id}_{summary}_{timestamp}.json
    // Thread ID is a UUID (36 chars with dashes)
    let fname = backup_path
        .file_stem()
        .and_then(|f| f.to_str())
        .unwrap_or("");

    let thread_id = if fname.len() >= 36 && fname.chars().nth(8) == Some('-') {
        &fname[..36]
    } else {
        return Err(format!(
            "Cannot extract thread ID from filename '{}'. Expected format: {{uuid}}_{{summary}}_{{timestamp}}.json",
            backup_path.display()
        ));
    };

    println!("  Thread ID: {}", thread_id);
    println!("  JSON size: {}", format_bytes(json_str.len()));

    let db_path = get_db_path()?;
    let conn = db::open_db(&db_path).map_err(|e| format!("Failed to open DB: {}", e))?;

    // Verify thread exists
    let exists: bool = conn
        .prepare("SELECT COUNT(*) FROM threads WHERE id = ?")
        .and_then(|mut stmt| stmt.query_row([thread_id], |row| row.get::<_, i64>(0)))
        .map(|c| c > 0)
        .map_err(|e| format!("DB query error: {}", e))?;

    if !exists {
        return Err(format!("Thread '{}' not found in database", thread_id));
    }

    // Compress and write
    let compressed = zstd::stream::encode_all(json_str.as_bytes(), 3)
        .map_err(|e| format!("Compression failed: {}", e))?;

    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE threads SET data = ?, data_type = 'zstd', updated_at = ? WHERE id = ?",
        rusqlite::params![compressed, now, thread_id],
    )
    .map_err(|e| format!("Failed to update DB: {}", e))?;

    println!();
    println!(
        "  Restored! ({} -> {} compressed)",
        format_bytes(json_str.len()),
        format_bytes(compressed.len())
    );
    println!();
    println!("You can now open Zed. Context window size will update after the first message.");

    Ok(())
}

fn run_cli(args: &[String]) -> Result<(), String> {
    let command = args[1].to_lowercase();

    match command.as_str() {
        "list" => cmd_list(),
        "clean" => {
            if args.len() < 3 {
                return Err(
                    "Usage: zed-context-cleaner clean <thread_id_or_title> [-n N]".to_string(),
                );
            }
            let query = &args[2];
            let mut keep_last_n: usize = 10;

            // Parse optional -n flag
            let mut i = 3;
            while i < args.len() {
                if args[i] == "-n" && i + 1 < args.len() {
                    keep_last_n = args[i + 1]
                        .parse()
                        .map_err(|_| format!("Invalid number: {}", args[i + 1]))?;
                    i += 2;
                } else {
                    return Err(format!("Unknown argument: {}", args[i]));
                }
            }

            let thread_id = resolve_thread_id(query)?;
            cmd_clean(&thread_id, keep_last_n)
        }
        "restore" => {
            if args.len() < 3 {
                return Err("Usage: zed-context-cleaner restore <backup_file>".to_string());
            }
            cmd_restore(&args[2])
        }
        "version" | "--version" | "-v" | "-V" => {
            println!("zed-context-cleaner v{}", VERSION);
            Ok(())
        }
        "help" | "--help" | "-h" => {
            print_usage();
            Ok(())
        }
        _ => Err(format!(
            "Unknown command: '{}'. Use 'help' for usage.",
            command
        )),
    }
}

fn run_gui() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("Zed Context Cleaner v{}", VERSION))
            .with_inner_size([1100.0, 864.0])
            .with_min_inner_size([800.0, 600.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Zed Context Cleaner",
        native_options,
        Box::new(|cc| Box::new(app::ZedContextCleanerApp::new(cc))),
    )
}

fn main() {
    setup_logging();

    let args: Vec<String> = std::env::args().collect();

    if args.len() > 1 {
        // CLI mode
        match run_cli(&args) {
            Ok(()) => {}
            Err(e) => {
                eprintln!("Error: {}", e);
                eprintln!();
                print_usage();
                std::process::exit(1);
            }
        }
    } else {
        // GUI mode
        if let Err(e) = run_gui() {
            eprintln!("GUI error: {}", e);
            std::process::exit(1);
        }
    }
}

