# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2025-04-16

### Added

- **GUI application** (egui/eframe) for browsing and cleaning Zed AI chat threads
- **CLI mode** with commands: `list`, `clean`, `restore`, `help`
- Thread list with search and filter (All / Chat / Subagent)
- Thread type detection: Chat (main threads) vs Subagent (spawned by `spawn_agent`)
- Thread statistics: message counts, size breakdown (thinking/tool_results/text), top tools
- Cleanup preview with estimated size reduction before applying
- **Cleanup operations:**
  - Remove `Thinking` and `RedactedThinking` blocks from non-protected agent messages
  - Remove `reasoning_details` from non-protected agent messages
  - Truncate `terminal`, `ssh_run`, `ssh_connect` tool results to 2000 bytes
  - Truncate `read_file` tool results to 3000 bytes
  - Truncate `grep`, `find_path`, `list_directory` tool results to 2000 bytes
  - Preserve `edit_file`, `create_directory`, `copy_path`, `move_path`, `delete_path`, `save_file`, `spawn_agent` results in full
  - Deduplicate `read_file` results (keep only last call per file path)
  - Null out `initial_project_snapshot`
  - Recalculate `request_token_usage` and `cumulative_token_usage`
- Configurable "Keep last N dialogs" slider (0-50, default 10)
- Automatic backup before cleanup (JSON file in `backups/` directory)
- Backup filenames include thread ID, title, and timestamp
- Restore from backup (GUI modal and CLI command)
- Confirmation dialog with Zed close warning before cleanup
- Processing spinner during background cleanup
- Cleanup result summary (before/after sizes, reduction percentage)
- Thread ID display with copy button
- File logging to `zed-context-cleaner.log`
- Auto-detection of `threads.db` path per OS (Windows, macOS, Linux)
- Manual DB file selection via file dialog
- CLI search by thread title (case-insensitive substring match)
- 8 unit tests for cleanup logic

### Supported Platforms

- Windows (primary development platform)
- macOS
- Linux