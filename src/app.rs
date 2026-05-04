use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;

use rusqlite::Connection;

use crate::cleaner::{self, CleanConfig};
use crate::db;
use crate::types::*;

/// Filter for the thread list
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ThreadFilter {
    All,
    Chat,
    Subagent,
}

impl ThreadFilter {
    fn label(&self) -> &'static str {
        match self {
            ThreadFilter::All => "All",
            ThreadFilter::Chat => "Chat",
            ThreadFilter::Subagent => "Subagent",
        }
    }
}

/// State for the confirmation dialog before cleanup
#[derive(Debug, Clone, PartialEq)]
enum DialogState {
    None,
    ConfirmCleanup,
    Processing {
        started: std::time::Instant,
    },
    CleanupResult {
        old_size: usize,
        new_size: usize,
        backup_file: String,
        thread_id: String,
    },
    CleanupError {
        message: String,
    },
    RestoreList,
    ConfirmRestore {
        backup_path: PathBuf,
        thread_id: String,
    },
    RestoreResult {
        message: String,
    },
}

/// Result sent back from the background cleanup thread
struct CleanupDone {
    old_size: usize,
    new_size: usize,
    backup_file: String,
    cleaned_thread: DbThread,
    thread_id: String,
    data_type: String,
}

pub struct ZedContextCleanerApp {
    db_path: Option<PathBuf>,
    db_conn: Option<Connection>,

    thread_list: Vec<ThreadMeta>,
    selected_thread_id: Option<String>,

    loaded_thread: Option<DbThread>,
    loaded_raw_json: Option<String>,
    loaded_data_type: Option<String>,
    thread_stats: Option<ThreadStats>,

    keep_last_n: usize,

    cleanup_preview: Option<CleanupPreview>,
    preview_dirty: bool,

    status_message: String,
    status_is_error: bool,

    file_dialog_open: bool,

    /// Cached thread type: thread_id -> "Subagent" | "Chat"
    type_cache: HashMap<String, String>,

    /// Current filter selection
    filter: ThreadFilter,

    /// Search/filter text
    search_text: String,

    /// Modal dialog state
    dialog: DialogState,

    /// Available backup files for the selected thread: (filename, path, date, size)
    backup_list: Vec<(String, PathBuf, String, String)>,

    /// Channel receiver for background cleanup result
    cleanup_rx: Option<mpsc::Receiver<Result<CleanupDone, String>>>,

    /// Skip polling on the frame cleanup was started
    cleanup_started_this_frame: bool,

    /// Per-tool-category analysis for the currently loaded thread
    thread_analysis: Option<ThreadAnalysis>,

    /// Checkbox state: tool_name -> enabled (true = will be cleaned)
    category_checks: HashMap<String, bool>,

    /// Remove large images/files from old User messages
    remove_large_images: bool,

    /// Nullify raw_input and input in ToolUse blocks
    strip_tool_inputs: bool,

    /// Nullify output field in tool_results
    strip_tool_output: bool,

    /// Remove Agent messages that contain only ToolUse (no Text)
    remove_tool_only_messages: bool,

    /// Fix GPT->Claude switch error by removing RedactedThinking from all messages
    fix_redacted_thinking: bool,
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

/// Detect thread type from the DbThread JSON structure.
/// "Subagent" = subagent_context is non-null (spawned by another thread via spawn_agent).
/// "Chat" = main thread started by the user (regardless of tool usage).
fn detect_thread_type(thread: &DbThread) -> String {
    if let Some(ref ctx) = thread.subagent_context {
        if !ctx.is_null() {
            return "Subagent".to_string();
        }
    }
    "Chat".to_string()
}

impl ZedContextCleanerApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        log::info!("Initializing ZedContextCleanerApp");

        let mut app = Self {
            db_path: None,
            db_conn: None,
            thread_list: Vec::new(),
            selected_thread_id: None,
            loaded_thread: None,
            loaded_raw_json: None,
            loaded_data_type: None,
            thread_stats: None,
            keep_last_n: 10,
            cleanup_preview: None,
            preview_dirty: false,
            status_message: String::new(),
            status_is_error: false,
            file_dialog_open: false,
            type_cache: HashMap::new(),
            filter: ThreadFilter::All,
            search_text: String::new(),
            dialog: DialogState::None,
            backup_list: Vec::new(),
            cleanup_rx: None,
            cleanup_started_this_frame: false,
            thread_analysis: None,
            category_checks: HashMap::new(),
            remove_large_images: true,
            strip_tool_inputs: true,
            strip_tool_output: true,
            remove_tool_only_messages: false,
            fix_redacted_thinking: false,
        };

        if let Some(path) = db::default_db_path() {
            log::info!("Default DB path: {}", path.display());
            if path.exists() {
                app.db_path = Some(path.clone());
                match db::open_db(&path) {
                    Ok(conn) => {
                        log::info!("Successfully opened DB at {}", path.display());
                        match db::load_thread_list(&conn) {
                            Ok(list) => {
                                log::info!("Loaded {} threads from DB", list.len());
                                app.status_message =
                                    format!("Loaded {} threads from default DB", list.len());
                                app.status_is_error = false;
                                for meta in &list {
                                    if !meta.thread_type.is_empty() {
                                        app.type_cache
                                            .insert(meta.id.clone(), meta.thread_type.clone());
                                    }
                                }
                                app.thread_list = list;
                            }
                            Err(e) => {
                                log::error!("Failed to load thread list: {}", e);
                                app.status_message = format!("Failed to load thread list: {}", e);
                                app.status_is_error = true;
                            }
                        }
                        app.db_conn = Some(conn);
                    }
                    Err(e) => {
                        log::error!("Failed to open DB at {}: {}", path.display(), e);
                        app.status_message = format!("Failed to open DB: {}", e);
                        app.status_is_error = true;
                    }
                }
            } else {
                log::info!("Default DB not found at {}", path.display());
                app.status_message =
                    "Default DB not found. Use \u{1f4c2} to choose one.".to_string();
                app.status_is_error = false;
            }
        } else {
            log::error!("Could not determine default DB path for this OS");
            app.status_message = "Could not determine default DB path.".to_string();
            app.status_is_error = true;
        }

        app
    }

    fn reconnect_db(&mut self) {
        log::info!("Reconnecting to DB");
        self.db_conn = None;
        self.thread_list.clear();
        self.selected_thread_id = None;
        self.loaded_thread = None;
        self.loaded_raw_json = None;
        self.loaded_data_type = None;
        self.thread_stats = None;
        self.cleanup_preview = None;
        self.preview_dirty = false;
        self.type_cache.clear();
        self.thread_analysis = None;
        self.category_checks.clear();

        let Some(path) = &self.db_path else {
            log::error!("No database path set");
            self.status_message = "No database path set.".to_string();
            self.status_is_error = true;
            return;
        };

        log::info!("Opening DB at {}", path.display());
        match db::open_db(path) {
            Ok(conn) => match db::load_thread_list(&conn) {
                Ok(list) => {
                    log::info!("Loaded {} threads after reconnect", list.len());
                    self.status_message = format!("Loaded {} threads", list.len());
                    self.status_is_error = false;
                    for meta in &list {
                        if !meta.thread_type.is_empty() {
                            self.type_cache
                                .insert(meta.id.clone(), meta.thread_type.clone());
                        }
                    }
                    self.thread_list = list;
                    self.db_conn = Some(conn);
                }
                Err(e) => {
                    log::error!("Failed to load thread list after reconnect: {}", e);
                    self.status_message = format!("Failed to load thread list: {}", e);
                    self.status_is_error = true;
                    self.db_conn = Some(conn);
                }
            },
            Err(e) => {
                log::error!("Failed to open DB: {}", e);
                self.status_message = format!("Failed to open DB: {}", e);
                self.status_is_error = true;
            }
        }
    }

    fn choose_db_file(&mut self) {
        log::info!("Opening file dialog to choose DB");
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("SQLite DB", &["db"])
            .pick_file()
        {
            log::info!("User selected DB: {}", path.display());
            self.db_path = Some(path);
            self.reconnect_db();
        } else {
            log::info!("File dialog cancelled");
        }
    }

    fn refresh(&mut self) {
        log::info!("Refreshing thread list");
        if let Some(conn) = &self.db_conn {
            match db::load_thread_list(conn) {
                Ok(list) => {
                    log::info!("Refreshed: {} threads", list.len());
                    self.status_message = format!("Refreshed: {} threads", list.len());
                    self.status_is_error = false;
                    for meta in &list {
                        if !meta.thread_type.is_empty() {
                            self.type_cache
                                .insert(meta.id.clone(), meta.thread_type.clone());
                        }
                    }
                    self.thread_list = list;
                }
                Err(e) => {
                    log::error!("Refresh failed: {}", e);
                    self.status_message = format!("Refresh failed: {}", e);
                    self.status_is_error = true;
                }
            }
        } else {
            log::error!("Refresh attempted with no DB connection");
            self.status_message = "No database connection.".to_string();
            self.status_is_error = true;
        }
    }

    fn select_thread(&mut self, id: String) {
        log::info!("Selecting thread: {}", id);

        let Some(conn) = &self.db_conn else {
            log::error!("select_thread: no DB connection");
            self.status_message = "No database connection.".to_string();
            self.status_is_error = true;
            return;
        };

        match db::load_thread(conn, &id) {
            Ok((thread, raw_json, data_type)) => {
                log::info!(
                    "Thread loaded: id={}, data_type={}, json_len={}, messages={}",
                    id,
                    data_type,
                    raw_json.len(),
                    thread.messages.len()
                );

                let thread_type = detect_thread_type(&thread);
                log::info!("Thread type detected: {}", thread_type);
                self.type_cache.insert(id.clone(), thread_type);

                let compressed_size = self
                    .thread_list
                    .iter()
                    .find(|t| t.id == id)
                    .map(|t| t.data_size)
                    .unwrap_or(0);

                log::info!("Computing stats for thread {}", id);
                let stats = cleaner::compute_stats(&thread, &raw_json, compressed_size);
                log::info!(
                    "Stats: total_msgs={}, user={}, agent={}, thinking_bytes={}, tool_results_bytes={}, text_bytes={}",
                    stats.total_messages,
                    stats.user_messages,
                    stats.agent_messages,
                    stats.thinking_bytes,
                    stats.tool_results_bytes,
                    stats.text_bytes
                );

                self.thread_stats = Some(stats);

                // Compute per-tool-category analysis
                let analysis = cleaner::analyze_thread(&thread, self.keep_last_n);
                let checks: HashMap<String, bool> = analysis
                    .categories
                    .iter()
                    .map(|c| (c.tool_name.clone(), c.cleanable_bytes > 10_000))
                    .collect();
                self.category_checks = checks;
                self.thread_analysis = Some(analysis);

                self.loaded_thread = Some(thread);
                self.loaded_raw_json = Some(raw_json);
                self.loaded_data_type = Some(data_type);
                self.selected_thread_id = Some(id);
                self.preview_dirty = true;
                self.refresh_backup_list();
                self.status_message = "Thread loaded.".to_string();
                self.status_is_error = false;
            }
            Err(e) => {
                log::error!("Failed to load thread {}: {:?}", id, e);
                self.status_message = format!("Failed to load thread: {}", e);
                self.status_is_error = true;
            }
        }
    }

    fn get_backup_dir() -> Option<PathBuf> {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("backups")))
    }

    /// Scan backup directory for backups matching the selected thread
    fn refresh_backup_list(&mut self) {
        self.backup_list.clear();
        let Some(ref thread_id) = self.selected_thread_id else {
            return;
        };
        let Some(backup_dir) = Self::get_backup_dir() else {
            return;
        };
        if !backup_dir.exists() {
            return;
        }

        let entries = match std::fs::read_dir(&backup_dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let fname = path
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or("")
                .to_string();
            // Match by full thread_id in filename
            if fname.contains(thread_id) {
                let meta = std::fs::metadata(&path);
                let size_str = meta
                    .as_ref()
                    .ok()
                    .map(|m| format_bytes(m.len() as usize))
                    .unwrap_or_default();
                let date_str = meta
                    .as_ref()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(|t| {
                        let dt: chrono::DateTime<chrono::Local> = t.into();
                        dt.format("%Y-%m-%d %H:%M:%S").to_string()
                    })
                    .unwrap_or_default();
                self.backup_list.push((fname, path, date_str, size_str));
            }
        }
        // Sort newest first
        self.backup_list.sort_by(|a, b| b.0.cmp(&a.0));
    }

    fn start_backup_and_cleanup(&mut self, ctx: &egui::Context) {
        log::info!("Starting backup & cleanup (background)");
        log::info!(
            "Config: keep_last_n={} fix_redacted_thinking={} strip_inputs={} strip_output={}",
            self.keep_last_n,
            self.fix_redacted_thinking,
            self.strip_tool_inputs,
            self.strip_tool_output
        );

        let thread_id = match &self.selected_thread_id {
            Some(id) => id.clone(),
            None => {
                self.dialog = DialogState::CleanupError {
                    message: "No thread selected.".to_string(),
                };
                return;
            }
        };
        let raw_json = match &self.loaded_raw_json {
            Some(r) => r.clone(),
            None => {
                self.dialog = DialogState::CleanupError {
                    message: "No thread data loaded.".to_string(),
                };
                return;
            }
        };
        let data_type = match &self.loaded_data_type {
            Some(dt) => dt.clone(),
            None => {
                self.dialog = DialogState::CleanupError {
                    message: "No data type information.".to_string(),
                };
                return;
            }
        };
        let thread = match &self.loaded_thread {
            Some(t) => t.clone(),
            None => {
                self.dialog = DialogState::CleanupError {
                    message: "No thread loaded.".to_string(),
                };
                return;
            }
        };

        let keep_last_n = self.keep_last_n;
        let remove_large_images = self.remove_large_images;
        let strip_tool_inputs = self.strip_tool_inputs;
        let strip_tool_output = self.strip_tool_output;
        let remove_tool_only_messages = self.remove_tool_only_messages;
        let fix_redacted_thinking = self.fix_redacted_thinking;
        let skip_tool_names: Vec<String> = self
            .category_checks
            .iter()
            .filter(|(_, &checked)| !checked)
            .map(|(name, _)| name.clone())
            .collect();

        let (tx, rx) = mpsc::channel();
        self.cleanup_rx = Some(rx);
        self.dialog = DialogState::Processing {
            started: std::time::Instant::now(),
        };
        self.cleanup_started_this_frame = true;

        let repaint_ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<CleanupDone, String> {
                let old_size = raw_json.len();

                // Create backup directory
                let backup_dir = std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.join("backups")))
                    .ok_or("Cannot determine exe directory")?;

                std::fs::create_dir_all(&backup_dir)
                    .map_err(|e| format!("Cannot create backup dir: {}", e))?;

                // Build backup filename
                let summary_part = thread.title.as_deref().unwrap_or("untitled");
                let summary_safe: String = summary_part
                    .chars()
                    .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_' || *c == ' ')
                    .take(40)
                    .collect::<String>()
                    .trim()
                    .replace(' ', "_");
                let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
                let backup_filename = format!("{}_{}_{}.json", thread_id, summary_safe, timestamp);
                let backup_path = backup_dir.join(&backup_filename);

                // Always write pretty JSON backup regardless of DB format
                let pretty_backup = {
                    let v: serde_json::Value = serde_json::from_str(&raw_json)
                        .map_err(|e| format!("Failed to parse JSON: {}", e))?;
                    serde_json::to_string_pretty(&v)
                        .map_err(|e| format!("Failed to format JSON: {}", e))?
                };
                std::fs::write(&backup_path, &pretty_backup)
                    .map_err(|e| format!("Failed to write backup: {}", e))?;

                // Clean
                let config = CleanConfig {
                    keep_last_n_dialogs: keep_last_n,
                    skip_tool_names,
                    remove_large_images,
                    strip_tool_inputs,
                    strip_tool_output,
                    remove_tool_only_messages,
                    fix_redacted_thinking,
                    ..Default::default()
                };
                let cleaned = cleaner::clean_thread(&thread, &config);

                let new_size = serde_json::to_string_pretty(&cleaned)
                    .map(|s| s.len())
                    .unwrap_or(0);

                Ok(CleanupDone {
                    old_size,
                    new_size,
                    backup_file: backup_filename,
                    cleaned_thread: cleaned,
                    thread_id: thread_id.clone(),
                    data_type: data_type.clone(),
                })
            })();
            let _ = tx.send(result);
            repaint_ctx.request_repaint();
        });
    }

    /// Called from update() to check if background cleanup finished
    fn poll_cleanup_result(&mut self) {
        let rx = match self.cleanup_rx.take() {
            Some(rx) => rx,
            None => return,
        };

        match rx.try_recv() {
            Ok(Ok(done)) => {
                // Save to DB (must happen on main thread because Connection is not Send)
                if let Some(conn) = &self.db_conn {
                    if let Err(e) = db::save_thread(
                        conn,
                        &done.thread_id,
                        &done.cleaned_thread,
                        &done.data_type,
                    ) {
                        log::error!("Failed to save cleaned thread: {:?}", e);
                        self.dialog = DialogState::CleanupError {
                            message: format!("Failed to save: {}", e),
                        };
                        return;
                    }
                }

                log::info!(
                    "Thread cleaned: {} -> {} (saved {})",
                    format_bytes(done.old_size),
                    format_bytes(done.new_size),
                    format_bytes(done.old_size.saturating_sub(done.new_size))
                );

                self.status_message = format!(
                    "Cleaned! {} -> {} (-{:.1}%)",
                    format_bytes(done.old_size),
                    format_bytes(done.new_size),
                    if done.old_size > 0 {
                        (done.old_size as f64 - done.new_size as f64) / done.old_size as f64 * 100.0
                    } else {
                        0.0
                    }
                );
                self.status_is_error = false;

                self.dialog = DialogState::CleanupResult {
                    old_size: done.old_size,
                    new_size: done.new_size,
                    backup_file: done.backup_file,
                    thread_id: done.thread_id.clone(),
                };
            }
            Ok(Err(err_msg)) => {
                log::error!("Cleanup failed: {}", err_msg);
                self.dialog = DialogState::CleanupError { message: err_msg };
            }
            Err(mpsc::TryRecvError::Empty) => {
                // Still working, put receiver back
                self.cleanup_rx = Some(rx);
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.dialog = DialogState::CleanupError {
                    message: "Cleanup thread crashed unexpectedly.".to_string(),
                };
            }
        }
    }

    fn do_restore_from_backup(&mut self, backup_path: &PathBuf, thread_id: &str) {
        log::info!(
            "Restoring thread {} from {}",
            thread_id,
            backup_path.display()
        );

        let json_str = match std::fs::read_to_string(backup_path) {
            Ok(s) => s,
            Err(e) => {
                self.status_message = format!("Failed to read backup: {}", e);
                self.status_is_error = true;
                self.dialog = DialogState::None;
                return;
            }
        };

        // Validate JSON
        let _: serde_json::Value = match serde_json::from_str(&json_str) {
            Ok(v) => v,
            Err(e) => {
                self.status_message = format!("Backup contains invalid JSON: {}", e);
                self.status_is_error = true;
                self.dialog = DialogState::None;
                return;
            }
        };

        // Write back as zstd
        let Some(conn) = &self.db_conn else {
            self.status_message = "No database connection.".to_string();
            self.status_is_error = true;
            self.dialog = DialogState::None;
            return;
        };

        let compressed = match zstd::stream::encode_all(json_str.as_bytes(), 3) {
            Ok(c) => c,
            Err(e) => {
                self.status_message = format!("Failed to compress: {}", e);
                self.status_is_error = true;
                self.dialog = DialogState::None;
                return;
            }
        };

        let now = chrono::Utc::now().to_rfc3339();
        if let Err(e) = conn.execute(
            "UPDATE threads SET data = ?, data_type = 'zstd', updated_at = ? WHERE id = ?",
            rusqlite::params![compressed, now, thread_id],
        ) {
            self.status_message = format!("Failed to restore: {}", e);
            self.status_is_error = true;
            self.dialog = DialogState::None;
            return;
        }

        let msg = format!(
            "Restored from backup ({}).\nYou can now open Zed. Context window size will update after the first message.",
            format_bytes(json_str.len())
        );

        self.dialog = DialogState::RestoreResult {
            message: msg.clone(),
        };
        self.status_message = "Thread restored from backup!".to_string();
        self.status_is_error = false;

        // Reload
        let id = thread_id.to_string();
        self.select_thread(id);
        self.refresh();
    }

    fn render_top_panel(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("DB:");
                if let Some(path) = &self.db_path {
                    ui.monospace(path.display().to_string());
                } else {
                    ui.label("No database");
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Refresh").clicked() {
                        self.refresh();
                    }
                    if ui.button("Choose DB...").clicked() {
                        self.file_dialog_open = true;
                    }
                });
            });
        });
    }

    fn render_bottom_panel(&self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let color = if self.status_is_error {
                    egui::Color32::from_rgb(220, 60, 60)
                } else {
                    egui::Color32::from_rgb(60, 180, 80)
                };
                ui.colored_label(color, &self.status_message);
            });
        });
    }

    fn render_left_panel(&mut self, ctx: &egui::Context) -> Option<String> {
        let mut clicked_id: Option<String> = None;

        egui::SidePanel::left("thread_list_panel")
            .min_width(280.0)
            .max_width(400.0)
            .show(ctx, |ui| {
                ui.heading("Threads");

                // Search box
                ui.horizontal(|ui| {
                    ui.label("Search:");
                    ui.text_edit_singleline(&mut self.search_text);
                });

                // Filter buttons
                ui.horizontal(|ui| {
                    ui.label("Filter:");
                    for f in &[
                        ThreadFilter::All,
                        ThreadFilter::Chat,
                        ThreadFilter::Subagent,
                    ] {
                        let selected = self.filter == *f;
                        let btn = egui::Button::new(egui::RichText::new(f.label()).small().color(
                            if selected {
                                egui::Color32::WHITE
                            } else {
                                ui.visuals().text_color()
                            },
                        ));
                        let btn = if selected {
                            btn.fill(egui::Color32::from_rgb(60, 100, 160))
                        } else {
                            btn
                        };
                        if ui.add(btn).clicked() {
                            self.filter = *f;
                        }
                    }
                });
                ui.separator();

                let search_lower = self.search_text.to_lowercase();

                egui::ScrollArea::vertical().show(ui, |ui| {
                    for meta in &self.thread_list {
                        // Apply search filter
                        if !self.search_text.is_empty()
                            && !meta.summary.to_lowercase().contains(&search_lower)
                        {
                            continue;
                        }

                        // Apply type filter
                        match self.filter {
                            ThreadFilter::All => {}
                            ThreadFilter::Chat => {
                                match self.type_cache.get(&meta.id).map(|s| s.as_str()) {
                                    Some("Chat") => {}
                                    Some(_) => continue,
                                    None => continue,
                                }
                            }
                            ThreadFilter::Subagent => {
                                match self.type_cache.get(&meta.id).map(|s| s.as_str()) {
                                    Some("Subagent") => {}
                                    Some(_) => continue,
                                    None => continue,
                                }
                            }
                        }
                        let is_selected = self
                            .selected_thread_id
                            .as_ref()
                            .map(|s| s == &meta.id)
                            .unwrap_or(false);

                        let summary = if meta.summary.len() > 60 {
                            format!(
                                "{}...",
                                &meta.summary[..meta
                                    .summary
                                    .char_indices()
                                    .nth(57)
                                    .map(|(i, _)| i)
                                    .unwrap_or(meta.summary.len())]
                            )
                        } else if meta.summary.is_empty() {
                            "(no summary)".to_string()
                        } else {
                            meta.summary.clone()
                        };

                        let size_str = format_bytes(meta.data_size);

                        let response =
                            ui.allocate_ui(egui::vec2(ui.available_width(), 56.0), |ui| {
                                let rect = ui.max_rect();

                                if is_selected {
                                    ui.painter().rect_filled(
                                        rect,
                                        4.0,
                                        egui::Color32::from_rgb(60, 100, 160),
                                    );
                                }

                                ui.allocate_ui_at_rect(rect.shrink(4.0), |ui| {
                                    ui.vertical(|ui| {
                                        // First row: type badge + summary
                                        ui.horizontal(|ui| {
                                            // Show badge from cache for ALL threads (not just selected)
                                            let badge_text =
                                                self.type_cache.get(&meta.id).map(|s| s.as_str());

                                            if let Some(badge_text) = badge_text {
                                                let (badge_bg, badge_fg) = match badge_text {
                                                    "Subagent" => (
                                                        egui::Color32::from_rgb(120, 80, 160),
                                                        egui::Color32::WHITE,
                                                    ),
                                                    "Chat" => (
                                                        egui::Color32::from_rgb(60, 140, 80),
                                                        egui::Color32::WHITE,
                                                    ),
                                                    _ => {
                                                        (egui::Color32::GRAY, egui::Color32::WHITE)
                                                    }
                                                };
                                                ui.allocate_ui(egui::vec2(48.0, 16.0), |ui| {
                                                    let (rect, _) = ui.allocate_exact_size(
                                                        egui::vec2(48.0, 16.0),
                                                        egui::Sense::hover(),
                                                    );
                                                    ui.painter().rect_filled(rect, 3.0, badge_bg);
                                                    ui.painter().text(
                                                        rect.center(),
                                                        egui::Align2::CENTER_CENTER,
                                                        badge_text,
                                                        egui::FontId::proportional(10.0),
                                                        badge_fg,
                                                    );
                                                });
                                            }

                                            ui.label(egui::RichText::new(&summary).strong().color(
                                                if is_selected {
                                                    egui::Color32::WHITE
                                                } else {
                                                    ui.visuals().text_color()
                                                },
                                            ));
                                        });

                                        // Second row: date + size
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                egui::RichText::new(&meta.updated_at)
                                                    .small()
                                                    .color(if is_selected {
                                                        egui::Color32::from_rgb(200, 210, 230)
                                                    } else {
                                                        egui::Color32::GRAY
                                                    }),
                                            );
                                            ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    ui.label(
                                                        egui::RichText::new(&size_str)
                                                            .small()
                                                            .color(if is_selected {
                                                                egui::Color32::from_rgb(
                                                                    200, 210, 230,
                                                                )
                                                            } else {
                                                                egui::Color32::GRAY
                                                            }),
                                                    );
                                                },
                                            );
                                        });
                                    });
                                });

                                // Sense click on the entire row
                                let response =
                                    ui.interact(rect, ui.id().with(&meta.id), egui::Sense::click());
                                if response.clicked() {
                                    clicked_id = Some(meta.id.clone());
                                }
                            });

                        let _ = response;
                        ui.add_space(2.0);
                    }
                });
            });

        clicked_id
    }

    fn render_central_panel(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.selected_thread_id.is_none() {
                ui.centered_and_justified(|ui| {
                    ui.heading("<- Select a thread");
                });
                return;
            }

            egui::ScrollArea::vertical().show(ui, |ui| {
                // Thread ID with copy button
                if let Some(ref id) = self.selected_thread_id {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("ID:")
                                .small()
                                .color(egui::Color32::GRAY),
                        );
                        ui.label(
                            egui::RichText::new(id)
                                .small()
                                .monospace()
                                .color(egui::Color32::GRAY),
                        );
                        if ui.small_button("Copy").on_hover_text("Copy ID").clicked() {
                            ui.output_mut(|o| o.copied_text = id.clone());
                        }
                    });
                }

                // Title + type badge
                if let Some(thread) = &self.loaded_thread {
                    let title = thread.title.as_deref().unwrap_or("Untitled Thread");
                    ui.horizontal(|ui| {
                        ui.heading(egui::RichText::new(title).size(22.0));

                        let thread_type = self
                            .selected_thread_id
                            .as_ref()
                            .and_then(|id| self.type_cache.get(id));
                        if let Some(thread_type) = thread_type {
                            let (badge_bg, badge_fg) = match thread_type.as_str() {
                                "Subagent" => {
                                    (egui::Color32::from_rgb(120, 80, 160), egui::Color32::WHITE)
                                }
                                "Chat" => {
                                    (egui::Color32::from_rgb(60, 140, 80), egui::Color32::WHITE)
                                }
                                _ => (egui::Color32::GRAY, egui::Color32::WHITE),
                            };
                            ui.allocate_ui(egui::vec2(60.0, 22.0), |ui| {
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(60.0, 22.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().rect_filled(rect, 4.0, badge_bg);
                                ui.painter().text(
                                    rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    thread_type,
                                    egui::FontId::proportional(13.0),
                                    badge_fg,
                                );
                            });
                        }
                    });
                }
                ui.separator();

                // Statistics
                if let Some(stats) = &self.thread_stats {
                    ui.heading("Statistics");
                    ui.add_space(4.0);

                    egui::Grid::new("stats_grid")
                        .num_columns(2)
                        .spacing([20.0, 4.0])
                        .show(ui, |ui| {
                            ui.label("Total messages:");
                            ui.label(format!("{}", stats.total_messages));
                            ui.end_row();

                            ui.label("User messages:");
                            ui.label(format!("{}", stats.user_messages));
                            ui.end_row();

                            ui.label("Agent messages:");
                            ui.label(format!("{}", stats.agent_messages));
                            ui.end_row();

                            ui.label("DB size (compressed):");
                            ui.label(format_bytes(stats.compressed_size));
                            ui.end_row();

                            ui.label("JSON size (uncompressed):");
                            ui.label(format_bytes(stats.uncompressed_size));
                            ui.end_row();

                            ui.label("Thinking data:");
                            let think_pct = if stats.uncompressed_size > 0 {
                                (stats.thinking_bytes as f64 / stats.uncompressed_size as f64)
                                    * 100.0
                            } else {
                                0.0
                            };
                            ui.label(format!(
                                "{} ({:.1}%)",
                                format_bytes(stats.thinking_bytes),
                                think_pct
                            ));
                            ui.end_row();

                            ui.label("Tool results:");
                            let tool_pct = if stats.uncompressed_size > 0 {
                                (stats.tool_results_bytes as f64 / stats.uncompressed_size as f64)
                                    * 100.0
                            } else {
                                0.0
                            };
                            ui.label(format!(
                                "{} ({:.1}%)",
                                format_bytes(stats.tool_results_bytes),
                                tool_pct
                            ));
                            ui.end_row();

                            ui.label("Text blocks:");
                            let text_pct = if stats.uncompressed_size > 0 {
                                (stats.text_bytes as f64 / stats.uncompressed_size as f64) * 100.0
                            } else {
                                0.0
                            };
                            ui.label(format!(
                                "{} ({:.1}%)",
                                format_bytes(stats.text_bytes),
                                text_pct
                            ));
                            ui.end_row();
                        });
                }

                ui.separator();

                // Cleanup Options
                ui.heading("Cleanup Options");
                ui.add_space(4.0);

                let old_keep = self.keep_last_n;
                ui.horizontal(|ui| {
                    ui.label("Keep last N dialogs:");
                    ui.add(egui::Slider::new(&mut self.keep_last_n, 0..=50));
                });
                if old_keep != self.keep_last_n {
                    self.preview_dirty = true;
                }

                ui.add_space(4.0);

                // Always cleaned (info)
                ui.label(
                    egui::RichText::new("Always removed: Thinking blocks, reasoning_details, initial_project_snapshot")
                        .strong(),
                );

                ui.add_space(2.0);
                ui.checkbox(&mut self.remove_large_images, "Remove large images/files from old messages");
                ui.checkbox(&mut self.strip_tool_inputs, "Strip tool inputs (raw_input, input) from old messages");
                ui.checkbox(&mut self.strip_tool_output, "Strip output field from tool results (duplicates content)");
                ui.checkbox(&mut self.remove_tool_only_messages, "Remove tool-only Agent messages (no text response)");
                ui.checkbox(&mut self.fix_redacted_thinking, "Fix GPT->Claude switch (remove RedactedThinking from all messages)");

                ui.add_space(6.0);

                // Tool category checkboxes
                if let Some(analysis) = self.thread_analysis.clone() {
                    ui.label(egui::RichText::new("Tool Results to Clean:").strong());
                    ui.add_space(4.0);

                    egui::Grid::new("category_grid")
                        .num_columns(4)
                        .min_col_width(8.0)
                        .spacing([10.0, 3.0])
                        .striped(true)
                        .show(ui, |ui| {
                            // Header
                            ui.label("");
                            ui.label(egui::RichText::new("Tool").strong().small());
                            ui.label(egui::RichText::new("Calls").strong().small());
                            ui.label(egui::RichText::new("Size (cleanable)").strong().small());
                            ui.end_row();

                            for cat in &analysis.categories {
                                if let Some(checked) =
                                    self.category_checks.get_mut(&cat.tool_name)
                                {
                                    ui.add(egui::Checkbox::without_text(checked));
                                } else {
                                    ui.label("");
                                }

                                ui.label(egui::RichText::new(&cat.tool_name).monospace().small());
                                ui.label(egui::RichText::new(format!("{}", cat.count)).small());

                                let size_text = if cat.cleanable_bytes > 0 {
                                    format!(
                                        "{} ({})",
                                        format_bytes(cat.total_bytes),
                                        format_bytes(cat.cleanable_bytes)
                                    )
                                } else {
                                    format!("{} (protected)", format_bytes(cat.total_bytes))
                                };
                                ui.label(egui::RichText::new(size_text).small());
                                ui.end_row();
                            }
                        });

                    // Accurate estimation: simulate full cleanup and compare sizes
                    let total_savings: usize = if let Some(thread) = &self.loaded_thread {
                        let skip_tool_names: Vec<String> = self
                            .category_checks
                            .iter()
                            .filter(|(_, enabled)| !**enabled)
                            .map(|(name, _)| name.clone())
                            .collect();
                        let sim_config = cleaner::CleanConfig {
                            keep_last_n_dialogs: self.keep_last_n,
                            skip_tool_names,
                            remove_large_images: self.remove_large_images,
                            strip_tool_inputs: self.strip_tool_inputs,
                            strip_tool_output: self.strip_tool_output,
                            remove_tool_only_messages: self.remove_tool_only_messages,
                            fix_redacted_thinking: self.fix_redacted_thinking,
                            ..Default::default()
                        };
                        let cleaned = cleaner::clean_thread(thread, &sim_config);
                        let cleaned_size = serde_json::to_string_pretty(&cleaned)
                            .map(|s| s.len())
                            .unwrap_or(0);
                        if let Some(stats) = &self.thread_stats {
                            stats.uncompressed_size.saturating_sub(cleaned_size)
                        } else {
                            0
                        }
                    } else {
                        0
                    };

                    ui.add_space(8.0);
                    if let Some(stats) = &self.thread_stats {
                        let new_est = stats.uncompressed_size.saturating_sub(total_savings);
                        let pct = if stats.uncompressed_size > 0 {
                            total_savings as f64 / stats.uncompressed_size as f64 * 100.0
                        } else {
                            0.0
                        };
                        ui.label(
                            egui::RichText::new(format!(
                                "Estimated: {} -> {} (-{:.1}%)",
                                format_bytes(stats.uncompressed_size),
                                format_bytes(new_est),
                                pct,
                            ))
                            .strong()
                            .color(if pct > 10.0 {
                                egui::Color32::from_rgb(60, 200, 120)
                            } else {
                                ui.visuals().text_color()
                            }),
                        );
                    }
                }

                ui.add_space(8.0);

                // Warning
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("/!\\ Close this thread in Zed before cleanup!")
                            .color(egui::Color32::from_rgb(220, 180, 50))
                            .strong(),
                    );
                });
                ui.add_space(4.0);

                // Buttons
                ui.horizontal(|ui| {
                    if ui
                        .add_sized(
                            [220.0, 36.0],
                            egui::Button::new(egui::RichText::new("Backup & Cleanup").size(16.0)),
                        )
                        .clicked()
                    {
                        self.dialog = DialogState::ConfirmCleanup;
                    }

                    if ui
                        .add_sized(
                            [180.0, 36.0],
                            egui::Button::new(
                                egui::RichText::new("Restore from Backup").size(14.0),
                            ),
                        )
                        .clicked()
                    {
                        self.refresh_backup_list();
                        self.dialog = DialogState::RestoreList;
                    }
                });
            });
        });
    }

    fn render_dialogs(&mut self, ctx: &egui::Context) {
        let dialog = self.dialog.clone();
        match dialog {
            DialogState::None => {}
            DialogState::Processing { .. } => {
                egui::Window::new("Processing...")
                    .collapsible(false)
                    .resizable(false)
                    .interactable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.add_space(16.0);
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.add_space(8.0);
                            ui.label(
                                egui::RichText::new("Cleaning thread, please wait...").size(16.0),
                            );
                        });
                        ui.add_space(16.0);
                    });
                ctx.request_repaint();
            }
            DialogState::CleanupError { ref message } => {
                let msg = message.clone();
                egui::Window::new("Cleanup Error")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("Cleanup failed:")
                                .strong()
                                .color(egui::Color32::from_rgb(220, 60, 60)),
                        );
                        ui.add_space(4.0);
                        ui.label(&msg);
                        ui.add_space(12.0);
                        if ui.button("OK").clicked() {
                            self.dialog = DialogState::None;
                        }
                    });
            }
            DialogState::ConfirmCleanup => {
                egui::Window::new("Confirm Cleanup")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("/!\\ Make sure this thread is CLOSED in Zed!")
                                .strong()
                                .size(16.0)
                                .color(egui::Color32::from_rgb(220, 180, 50)),
                        );
                        ui.add_space(8.0);
                        ui.label("Zed keeps the active thread in memory. If the thread is open,");
                        ui.label("Zed will overwrite the cleaned version when it next saves.");
                        ui.add_space(4.0);
                        ui.label("Steps: close the AI panel in Zed (or switch to another thread),");
                        ui.label("then click Proceed below.");
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("Proceed with Cleanup").strong(),
                                    )
                                    .fill(egui::Color32::from_rgb(60, 140, 80)),
                                )
                                .clicked()
                            {
                                self.dialog = DialogState::None;
                                self.start_backup_and_cleanup(ctx);
                            }
                            ui.add_space(16.0);
                            if ui.button("Cancel").clicked() {
                                self.dialog = DialogState::None;
                            }
                        });
                    });
            }
            DialogState::CleanupResult {
                old_size,
                new_size,
                ref backup_file,
                ref thread_id,
            } => {
                let _thread_id = thread_id;
                egui::Window::new("Cleanup Complete")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("Thread cleaned successfully!")
                                .strong()
                                .size(18.0)
                                .color(egui::Color32::from_rgb(60, 200, 80)),
                        );
                        ui.add_space(12.0);

                        egui::Grid::new("result_grid")
                            .num_columns(2)
                            .spacing([20.0, 6.0])
                            .show(ui, |ui| {
                                ui.label("Before:");
                                ui.label(
                                    egui::RichText::new(format_bytes(old_size)).strong(),
                                );
                                ui.end_row();

                                ui.label("After:");
                                ui.label(
                                    egui::RichText::new(format_bytes(new_size)).strong(),
                                );
                                ui.end_row();

                                let saved = old_size.saturating_sub(new_size);
                                let pct = if old_size > 0 {
                                    saved as f64 / old_size as f64 * 100.0
                                } else {
                                    0.0
                                };
                                ui.label("Saved:");
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} (-{:.1}%)",
                                        format_bytes(saved),
                                        pct
                                    ))
                                    .strong()
                                    .color(egui::Color32::from_rgb(60, 200, 80)),
                                );
                                ui.end_row();

                                ui.label("Backup:");
                                ui.label(
                                    egui::RichText::new(backup_file).small().monospace(),
                                );
                                ui.end_row();
                            });

                        ui.add_space(12.0);
                        ui.label(
                            egui::RichText::new(
                                "You can now open Zed. Context window size will update after the first message.",
                            )
                            .italics(),
                        );
                        ui.add_space(8.0);
                        if ui.button("OK").clicked() {
                            self.dialog = DialogState::None;
                        }
                    });
            }
            DialogState::RestoreList => {
                egui::Window::new("Restore from Backup")
                    .collapsible(false)
                    .resizable(true)
                    .default_width(500.0)
                    .default_height(350.0)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        if self.backup_list.is_empty() {
                            ui.add_space(16.0);
                            ui.label(
                                egui::RichText::new("No backups found for this thread.").size(16.0),
                            );
                            ui.add_space(4.0);
                            ui.label(
                                "Backups are created automatically when you run Backup & Cleanup.",
                            );
                            ui.add_space(16.0);
                            if ui.button("OK").clicked() {
                                self.dialog = DialogState::None;
                            }
                        } else {
                            ui.label(
                                egui::RichText::new(
                                    "/!\\ Close this thread in Zed before restoring!",
                                )
                                .strong()
                                .color(egui::Color32::from_rgb(220, 180, 50)),
                            );
                            ui.add_space(8.0);
                            ui.label(format!("{} backup(s) available:", self.backup_list.len()));
                            ui.add_space(4.0);

                            let mut restore_action: Option<(PathBuf, String)> = None;

                            egui::ScrollArea::vertical()
                                .max_height(250.0)
                                .show(ui, |ui| {
                                    for (fname, path, date_str, size_str) in &self.backup_list {
                                        let display = fname.trim_end_matches(".json");
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                egui::RichText::new(display).small().monospace(),
                                            );
                                            ui.label(
                                                egui::RichText::new(date_str)
                                                    .small()
                                                    .color(egui::Color32::GRAY),
                                            );
                                            ui.label(
                                                egui::RichText::new(size_str)
                                                    .small()
                                                    .color(egui::Color32::GRAY),
                                            );
                                            if ui.small_button("Restore").clicked() {
                                                if let Some(ref id) = self.selected_thread_id {
                                                    restore_action =
                                                        Some((path.clone(), id.clone()));
                                                }
                                            }
                                        });
                                        ui.separator();
                                    }
                                });

                            if let Some((path, tid)) = restore_action {
                                self.dialog = DialogState::ConfirmRestore {
                                    backup_path: path,
                                    thread_id: tid,
                                };
                            }

                            ui.add_space(8.0);
                            if ui.button("Cancel").clicked() {
                                self.dialog = DialogState::None;
                            }
                        }
                    });
            }
            DialogState::ConfirmRestore {
                ref backup_path,
                ref thread_id,
            } => {
                let bp = backup_path.clone();
                let tid = thread_id.clone();
                let fname = bp
                    .file_name()
                    .and_then(|f| f.to_str())
                    .unwrap_or("?")
                    .to_string();
                egui::Window::new("Confirm Restore")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("/!\\ Close this thread in Zed first!")
                                .strong()
                                .size(16.0)
                                .color(egui::Color32::from_rgb(220, 180, 50)),
                        );
                        ui.add_space(8.0);
                        ui.label(format!("Restore from: {}", fname));
                        ui.label("This will overwrite the current thread data in the database.");
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if ui
                                .add(
                                    egui::Button::new(egui::RichText::new("Restore").strong())
                                        .fill(egui::Color32::from_rgb(180, 100, 40)),
                                )
                                .clicked()
                            {
                                self.dialog = DialogState::None;
                                self.do_restore_from_backup(&bp, &tid);
                            }
                            ui.add_space(16.0);
                            if ui.button("Cancel").clicked() {
                                self.dialog = DialogState::None;
                            }
                        });
                    });
            }
            DialogState::RestoreResult { ref message } => {
                let msg = message.clone();
                egui::Window::new("Restore Complete")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("Thread restored!")
                                .strong()
                                .size(18.0)
                                .color(egui::Color32::from_rgb(60, 200, 80)),
                        );
                        ui.add_space(8.0);
                        ui.label(&msg);
                        ui.add_space(8.0);
                        if ui.button("OK").clicked() {
                            self.dialog = DialogState::None;
                        }
                    });
            }
        }
    }
}

impl eframe::App for ZedContextCleanerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Recalculate preview if dirty
        if self.preview_dirty {
            if let (Some(thread), Some(raw)) = (&self.loaded_thread, &self.loaded_raw_json) {
                let config = CleanConfig {
                    keep_last_n_dialogs: self.keep_last_n,
                    ..Default::default()
                };
                log::debug!(
                    "Recalculating preview with keep_last_n={}",
                    self.keep_last_n
                );
                self.cleanup_preview = Some(cleaner::preview_cleanup(thread, &config, raw));

                // Re-run analysis (keep_last_n affects protected zone / cleanable_bytes)
                let analysis = cleaner::analyze_thread(thread, self.keep_last_n);
                // Preserve existing checkbox states, only add new tools with smart default
                for cat in &analysis.categories {
                    self.category_checks
                        .entry(cat.tool_name.clone())
                        .or_insert(cat.cleanable_bytes > 10_000);
                }
                // Remove tools no longer present
                let tool_names: std::collections::HashSet<String> = analysis
                    .categories
                    .iter()
                    .map(|c| c.tool_name.clone())
                    .collect();
                self.category_checks.retain(|k, _| tool_names.contains(k));
                self.thread_analysis = Some(analysis);
            }
            self.preview_dirty = false;
        }

        // Handle file dialog (must be done before panels borrow self)
        let should_open_dialog = self.file_dialog_open;
        if should_open_dialog {
            self.file_dialog_open = false;
            self.choose_db_file();
        }

        self.render_top_panel(ctx);
        self.render_bottom_panel(ctx);
        let clicked_id = self.render_left_panel(ctx);
        self.render_central_panel(ctx);
        self.render_dialogs(ctx);

        // Poll background cleanup AFTER rendering so the spinner gets drawn first.
        // Show spinner for at least 500ms so user can see it.
        if self.cleanup_rx.is_some() {
            let min_display = std::time::Duration::from_millis(1000);
            let can_poll = match &self.dialog {
                DialogState::Processing { started } => started.elapsed() >= min_display,
                _ => true,
            };
            if can_poll && !self.cleanup_started_this_frame {
                self.poll_cleanup_result();
            }
            self.cleanup_started_this_frame = false;
            ctx.request_repaint();
        }

        // Handle thread selection after rendering (avoids borrow issues)
        if let Some(id) = clicked_id {
            self.select_thread(id);
        }
    }
}
