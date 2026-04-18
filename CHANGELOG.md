# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.1] - 2025-04-18

### Fixed

- **Estimated savings accuracy** — now calculates actual savings (`original_size - truncated_limit`) instead of counting full size of cleanable results
- **`edit_file` diffs now truncated** — removed from PRESERVE_TOOLS; large diffs (40-150 KB) are truncated to 2 KB like other tool results
- **Output field savings** — `output` field size now included in savings estimation

### Changed

- "Always removed" label is now bold with normal text color (was small gray)

## [0.2.0] - 2025-04-18

### Added

- **Remove large images/files** option — truncates base64 Image blocks and large Mention attachments in old User messages (> 10 KB), with checkbox in Cleanup Options
- **Tool category checkboxes** — per-tool breakdown with calls count, total size, cleanable size; user picks which tool results to clean
- **Dynamic savings estimation** — recalculates instantly when toggling checkboxes or moving the slider
- **Pretty JSON output** — DB writes and backups now use indented JSON matching Zed's native format
- **`--version` flag** in CLI, version displayed in GUI title bar
- **Screenshots** in README (main window, Zed error example)
- CLI: `clean` command accepts thread title (case-insensitive substring search), not just UUID

### Fixed

- **Zed compatibility: `invalid type null, expected u64`** — `TokenUsage` fields now use `skip_serializing_if = "Option::is_none"` so missing fields stay absent instead of becoming `null`
- **Zed compatibility: missing fields** — preserved `thought_signature` in ToolUse, `speed`, `thinking_enabled`, `thinking_effort`, `draft_prompt`, `ui_scroll_position`, `imported` in DbThread
- **Zed compatibility: `reasoning_details`** — removed entirely from JSON (not set to `null`) matching Python cleanup behavior
- **Zed compatibility: `content` field type** — `ToolResult.content` changed from `Option<String>` to `Option<Value>` to handle `{"Text": "..."}` format
- **Zed compatibility: unknown message types** — added `Other(Value)` fallback to `Message` enum for `Resume` and other unknown variants
- **Output field truncation** — `output` in tool results now truncated at `limit/2` (matching Python behavior)
- **`cumulative_token_usage` and `request_token_usage`** — never output as `null`, always as object with zeros or empty map
- **Subagent detection** — uses `subagent_context` field instead of heuristics
- Backup filenames now include full thread ID and sanitized title
- GUI: processing spinner shows during background cleanup (1s minimum display)
- GUI: OK button in cleanup result dialog no longer freezes UI
- GUI: removed emoji characters that rendered as squares on Windows

### Changed

- Simplified to two-panel layout (removed third "Thread Analysis" column)
- Thread type filter: "Chat" (main threads) and "Subagent" (spawned) instead of Agent/Chat
- "Always removed" label now bold, normal color (was small gray)

### Removed

- Top Tools bar chart (unnecessary clutter)
- Separate "Cleanup Selected Categories" button (merged into single "Backup & Cleanup")

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