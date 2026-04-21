use crate::types::{
    AgentContent, AgentMessage, CleanupPreview, DbThread, Message, ThreadAnalysis, ThreadStats,
    TokenUsage, ToolCategoryStats, ToolResult, ToolUseBlock,
};
use serde_json::Value;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Tool categories
// ---------------------------------------------------------------------------

const PRESERVE_TOOLS: &[&str] = &[
    "create_directory",
    "copy_path",
    "move_path",
    "delete_path",
    "spawn_agent",
    "save_file",
];

const TERMINAL_TOOLS: &[&str] = &[
    "terminal",
    "ssh_run",
    "ssh_connect",
    "ssh_disconnect",
    "ssh_status",
];

const SEARCH_TOOLS: &[&str] = &["grep", "find_path", "list_directory"];

// ---------------------------------------------------------------------------
// CleanConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CleanConfig {
    pub keep_last_n_dialogs: usize,
    pub terminal_limit: usize,
    pub read_file_limit: usize,
    pub search_limit: usize,
    /// Tool names to skip during cleanup (leave their results untouched).
    pub skip_tool_names: Vec<String>,
    /// Remove large images/files from old User messages
    pub remove_large_images: bool,
    /// Threshold in bytes for image/file removal (default 10KB)
    pub large_image_threshold: usize,
    /// Nullify `raw_input` and `input` in ToolUse blocks (old messages only)
    pub strip_tool_inputs: bool,
    /// Nullify `output` field in tool_results (it duplicates content)
    pub strip_tool_output: bool,
    /// Remove Agent messages that contain only ToolUse (no Text response)
    pub remove_tool_only_messages: bool,
}

impl Default for CleanConfig {
    fn default() -> Self {
        Self {
            keep_last_n_dialogs: 10,
            terminal_limit: 2000,
            read_file_limit: 3000,
            search_limit: 2000,
            skip_tool_names: Vec::new(),
            remove_large_images: true,
            large_image_threshold: 10_000,
            strip_tool_inputs: true,
            strip_tool_output: true,
            remove_tool_only_messages: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Tool byte limit
// ---------------------------------------------------------------------------

/// Returns `None` for tools that should be preserved in full, or `Some(limit)`
/// for tools whose results should be truncated.
pub fn tool_byte_limit(tool_name: &str, config: &CleanConfig) -> Option<usize> {
    if PRESERVE_TOOLS.contains(&tool_name) {
        None
    } else if TERMINAL_TOOLS.contains(&tool_name) {
        Some(config.terminal_limit)
    } else if SEARCH_TOOLS.contains(&tool_name) {
        Some(config.search_limit)
    } else if tool_name == "read_file" {
        Some(config.read_file_limit)
    } else {
        // Unknown tools get the terminal limit as a sensible default.
        Some(config.terminal_limit)
    }
}

// ---------------------------------------------------------------------------
// UTF-8 safe truncation
// ---------------------------------------------------------------------------

pub fn truncate_utf8(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let boundary = floor_char_boundary(s, max_bytes);
    let omitted = s.len() - boundary;
    format!(
        "{}\n... [TRUNCATED: {} bytes omitted]",
        &s[..boundary],
        omitted
    )
}

/// Equivalent to `str::floor_char_boundary` (stabilised in Rust 1.82).
/// Finds the largest byte index <= `index` that is a valid char boundary.
fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    let mut i = index;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

// ---------------------------------------------------------------------------
// read_file path extraction
// ---------------------------------------------------------------------------

pub fn extract_read_file_path(tool_use: &ToolUseBlock) -> Option<String> {
    tool_use
        .input
        .as_ref()
        .and_then(|v| v.get("path"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

// ---------------------------------------------------------------------------
// Main cleanup
// ---------------------------------------------------------------------------

pub fn clean_thread(thread: &DbThread, config: &CleanConfig) -> DbThread {
    // 1. Determine protected dialogs.
    //    A "dialog" is a consecutive User→Agent pair.
    let protected_set = compute_protected_indices(&thread.messages, config.keep_last_n_dialogs);

    // 2. Collect read_file tool-use info from non-protected agent messages for dedup.
    let duplicate_ids = compute_duplicate_read_file_ids(&thread.messages, &protected_set);

    // 3. Clean messages.
    let cleaned_messages: Vec<Message> = thread
        .messages
        .iter()
        .enumerate()
        .filter_map(|(idx, msg)| {
            if protected_set.contains(&idx) {
                return Some(msg.clone());
            }
            match msg {
                Message::User { user } => {
                    if config.remove_large_images {
                        Some(Message::User {
                            user: clean_user_message(user, config),
                        })
                    } else {
                        Some(msg.clone())
                    }
                }
                Message::Agent { agent } => {
                    // 5. Remove tool-only Agent messages (no Text block)
                    if config.remove_tool_only_messages {
                        let has_text = agent
                            .content
                            .iter()
                            .any(|c| matches!(c, AgentContent::Text(_)));
                        if !has_text {
                            return None; // Remove entirely
                        }
                    }
                    Some(Message::Agent {
                        agent: clean_agent_message(agent, config, &duplicate_ids),
                    })
                }
                Message::Other(_) => Some(msg.clone()),
            }
        })
        .collect();

    // 4. Collect remaining user message ids.
    let user_ids: std::collections::HashSet<String> = cleaned_messages
        .iter()
        .filter_map(|m| match m {
            Message::User { user: u } => Some(u.id.clone()),
            _ => None,
        })
        .collect();

    // 5. Filter request_token_usage — always produce Some, never None.
    let filtered_request_usage: Option<HashMap<String, TokenUsage>> = Some(
        thread
            .request_token_usage
            .as_ref()
            .map(|rtu| {
                rtu.iter()
                    .filter(|(k, _)| user_ids.contains(k.as_str()))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            })
            .unwrap_or_default(),
    );

    // 6. Recalculate cumulative — always produce an object, never null.
    let cumulative = Some(
        filtered_request_usage
            .as_ref()
            .map(|rtu| recalculate_cumulative(rtu))
            .unwrap_or_else(|| {
                serde_json::json!({
                    "input_tokens": 0,
                    "output_tokens": 0,
                    "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 0
                })
            }),
    );

    DbThread {
        title: thread.title.clone(),
        version: thread.version.clone(),
        updated_at: thread.updated_at.clone(),
        messages: cleaned_messages,
        detailed_summary: thread.detailed_summary.clone(),
        initial_project_snapshot: None,
        cumulative_token_usage: cumulative,
        request_token_usage: filtered_request_usage,
        model: thread.model.clone(),
        profile: thread.profile.clone(),
        subagent_context: thread.subagent_context.clone(),
        imported: thread.imported.clone(),
        speed: thread.speed.clone(),
        thinking_enabled: thread.thinking_enabled.clone(),
        thinking_effort: thread.thinking_effort.clone(),
        draft_prompt: thread.draft_prompt.clone(),
        ui_scroll_position: thread.ui_scroll_position.clone(),
    }
}

/// Returns a set of message indices that are "protected" (i.e. belong to the
/// last N complete User→Agent dialog pairs).
pub fn compute_protected_indices(
    messages: &[Message],
    keep_last_n: usize,
) -> std::collections::HashSet<usize> {
    // Collect (user_idx, agent_idx) pairs.
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < messages.len() {
        if matches!(&messages[i], Message::User { .. }) {
            // Look for the next Agent message.
            if i + 1 < messages.len() && matches!(&messages[i + 1], Message::Agent { .. }) {
                pairs.push((i, i + 1));
                i += 2;
                continue;
            }
        }
        i += 1;
    }

    let protected_pairs = if pairs.len() > keep_last_n {
        &pairs[pairs.len() - keep_last_n..]
    } else {
        &pairs[..]
    };

    let mut set = std::collections::HashSet::new();
    for &(u, a) in protected_pairs {
        set.insert(u);
        set.insert(a);
    }
    set
}

/// Returns a set of tool_use_ids whose read_file results should be replaced
/// with a deduplication notice (all but the last call per file path).
fn compute_duplicate_read_file_ids(
    messages: &[Message],
    protected: &std::collections::HashSet<usize>,
) -> std::collections::HashSet<String> {
    // path -> Vec<tool_use_id> in order of appearance
    let mut path_to_ids: HashMap<String, Vec<String>> = HashMap::new();

    for (idx, msg) in messages.iter().enumerate() {
        if protected.contains(&idx) {
            continue;
        }
        if let Message::Agent { agent } = msg {
            for content in &agent.content {
                if let AgentContent::ToolUse(tu) = content {
                    if tu.name == "read_file" {
                        if let Some(path) = extract_read_file_path(tu) {
                            path_to_ids.entry(path).or_default().push(tu.id.clone());
                        }
                    }
                }
            }
        }
    }

    let mut dup_ids = std::collections::HashSet::new();
    for (_path, ids) in &path_to_ids {
        if ids.len() > 1 {
            // Keep only the last one.
            for id in &ids[..ids.len() - 1] {
                dup_ids.insert(id.clone());
            }
        }
    }
    dup_ids
}

/// Remove large Image/Mention blocks from User messages (non-protected).
fn clean_user_message(
    user: &crate::types::UserMessage,
    config: &CleanConfig,
) -> crate::types::UserMessage {
    use crate::types::UserContent;

    let cleaned_content: Vec<UserContent> = user
        .content
        .iter()
        .map(|c| {
            match c {
                UserContent::Other(val) => {
                    // Check for {"Image": {"source": "base64..."}} pattern
                    if let Some(img) = val.get("Image") {
                        let size = serde_json::to_string(img).unwrap_or_default().len();
                        if size > config.large_image_threshold {
                            // Extract filename if possible
                            let label = img
                                .get("source")
                                .and_then(|s| s.as_str())
                                .map(|s| {
                                    if s.starts_with("iVBOR") {
                                        "PNG image"
                                    } else if s.starts_with("/9j/") {
                                        "JPEG image"
                                    } else if s.starts_with("R0lG") {
                                        "GIF image"
                                    } else {
                                        "image"
                                    }
                                })
                                .unwrap_or("image");
                            let placeholder = format!(
                                "[REMOVED: {}, original {} base64]",
                                label,
                                truncate_utf8_label(size)
                            );
                            return UserContent::Other(serde_json::json!({
                                "Image": {"source": placeholder}
                            }));
                        }
                    }
                    // Check for {"Mention": {"content": "huge text..."}} pattern
                    if let Some(mention) = val.get("Mention") {
                        if let Some(content) = mention.get("content").and_then(|c| c.as_str()) {
                            if content.len() > config.large_image_threshold {
                                let uri = mention
                                    .get("uri")
                                    .cloned()
                                    .unwrap_or(serde_json::Value::Null);
                                let placeholder = format!(
                                    "[CONTENT TRUNCATED: original {}]",
                                    truncate_utf8_label(content.len())
                                );
                                return UserContent::Other(serde_json::json!({
                                    "Mention": {
                                        "uri": uri,
                                        "content": placeholder
                                    }
                                }));
                            }
                        }
                    }
                    c.clone()
                }
                _ => c.clone(),
            }
        })
        .collect();

    crate::types::UserMessage {
        id: user.id.clone(),
        content: cleaned_content,
    }
}

/// Format byte size as human-readable label
fn truncate_utf8_label(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn clean_agent_message(
    agent: &AgentMessage,
    config: &CleanConfig,
    duplicate_ids: &std::collections::HashSet<String>,
) -> AgentMessage {
    // a. Filter content: remove Thinking and RedactedThinking.
    //    Optionally strip input/raw_input from ToolUse blocks.
    let cleaned_content: Vec<AgentContent> = agent
        .content
        .iter()
        .filter(|c| matches!(c, AgentContent::Text(_) | AgentContent::ToolUse(_)))
        .map(|c| {
            if config.strip_tool_inputs {
                if let AgentContent::ToolUse(tu) = c {
                    return AgentContent::ToolUse(ToolUseBlock {
                        id: tu.id.clone(),
                        name: tu.name.clone(),
                        raw_input: None,
                        input: None,
                        is_input_complete: tu.is_input_complete,
                        thought_signature: None,
                    });
                }
            }
            c.clone()
        })
        .collect();

    // b. reasoning_details — remove entirely (set to None, field skipped by skip_serializing_if)
    // c. Clean tool_results
    let cleaned_tool_results: Option<HashMap<String, ToolResult>> =
        agent.tool_results.as_ref().map(|tr| {
            tr.iter()
                .map(|(k, result)| {
                    let cleaned = clean_tool_result(result, config, duplicate_ids);
                    (k.clone(), cleaned)
                })
                .collect()
        });

    AgentMessage {
        content: cleaned_content,
        tool_results: cleaned_tool_results,
        reasoning_details: None, // None + skip_serializing_if = field absent in JSON
    }
}

/// Get the byte length of a content Value for statistics purposes.
fn content_value_len(v: &Value) -> usize {
    match v {
        Value::String(s) => s.len(),
        Value::Object(map) => {
            if let Some(Value::String(s)) = map.get("Text") {
                s.len()
            } else {
                serde_json::to_string(v).unwrap_or_default().len()
            }
        }
        Value::Null => 0,
        other => other.to_string().len(),
    }
}

/// Extract the text string from a ToolResult content Value.
/// Content can be:
///   - a plain string: "some text"
///   - an object like {"Text": "some text"}
///   - a list of blocks (serialized as JSON string for truncation)
///   - null
fn extract_content_text(content: &Value) -> Option<String> {
    match content {
        Value::String(s) => Some(s.clone()),
        Value::Object(map) => {
            if let Some(Value::String(s)) = map.get("Text") {
                Some(s.clone())
            } else {
                // Serialize the whole object as a string for truncation purposes
                Some(serde_json::to_string(content).unwrap_or_default())
            }
        }
        Value::Array(_) => Some(serde_json::to_string(content).unwrap_or_default()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

/// Re-wrap a truncated string back into the same shape as the original content Value.
fn rewrap_content(original: &Value, truncated: String) -> Value {
    match original {
        Value::String(_) => Value::String(truncated),
        Value::Object(map) if map.contains_key("Text") => {
            serde_json::json!({"Text": truncated})
        }
        _ => Value::String(truncated),
    }
}

fn clean_tool_result(
    result: &ToolResult,
    config: &CleanConfig,
    duplicate_ids: &std::collections::HashSet<String>,
) -> ToolResult {
    // If tool is in the skip list, leave it untouched.
    if config
        .skip_tool_names
        .iter()
        .any(|n| n == &result.tool_name)
    {
        return result.clone();
    }

    // Check if this is a duplicate read_file result.
    if duplicate_ids.contains(&result.tool_use_id) {
        return ToolResult {
            tool_use_id: result.tool_use_id.clone(),
            tool_name: result.tool_name.clone(),
            is_error: result.is_error,
            content: Some(Value::String(
                "[DUPLICATE READ_FILE RESULT REMOVED: see last call for this file]".to_string(),
            )),
            output: None, // Fix #4: clear output for duplicates too
        };
    }

    // Apply byte limit if applicable.
    let limit = tool_byte_limit(&result.tool_name, config);

    // Truncate content (same limit as Python)
    let cleaned_content = match (&result.content, limit) {
        (Some(content_val), Some(lim)) => {
            if let Some(text) = extract_content_text(content_val) {
                let truncated = truncate_utf8(&text, lim);
                Some(rewrap_content(content_val, truncated))
            } else {
                result.content.clone()
            }
        }
        (c, _) => c.clone(),
    };

    // #3: If strip_tool_output is enabled, just null the output entirely
    let cleaned_output = if config.strip_tool_output {
        None
    } else {
        // Truncate output at half the limit (same as Python: max_bytes // 2)
        match (&result.output, limit) {
            (Some(output_val), Some(lim)) => {
                let half = lim / 2;
                if let Some(text) = extract_content_text(output_val) {
                    if text.len() > half {
                        let truncated = truncate_utf8(&text, half);
                        Some(rewrap_content(output_val, truncated))
                    } else {
                        result.output.clone()
                    }
                } else {
                    let serialized = serde_json::to_string(output_val).unwrap_or_default();
                    if serialized.len() > half {
                        let truncated = truncate_utf8(&serialized, half);
                        Some(serde_json::Value::String(truncated))
                    } else {
                        result.output.clone()
                    }
                }
            }
            (o, _) => o.clone(),
        }
    };

    ToolResult {
        tool_use_id: result.tool_use_id.clone(),
        tool_name: result.tool_name.clone(),
        is_error: result.is_error,
        content: cleaned_content,
        output: cleaned_output,
    }
}

// ---------------------------------------------------------------------------
// Token usage recalculation
// ---------------------------------------------------------------------------

pub fn recalculate_cumulative(request_usage: &HashMap<String, TokenUsage>) -> Value {
    let mut input: u64 = 0;
    let mut output: u64 = 0;
    let mut cache_creation: u64 = 0;
    let mut cache_read: u64 = 0;

    for tu in request_usage.values() {
        input += tu.input_tokens.unwrap_or(0);
        output += tu.output_tokens.unwrap_or(0);
        cache_creation += tu.cache_creation_input_tokens.unwrap_or(0);
        cache_read += tu.cache_read_input_tokens.unwrap_or(0);
    }

    serde_json::json!({
        "input_tokens": input,
        "output_tokens": output,
        "cache_creation_input_tokens": cache_creation,
        "cache_read_input_tokens": cache_read,
    })
}

// ---------------------------------------------------------------------------
// Preview
// ---------------------------------------------------------------------------

pub fn preview_cleanup(thread: &DbThread, config: &CleanConfig, raw_json: &str) -> CleanupPreview {
    let cleaned = clean_thread(thread, config);
    let cleaned_json = serde_json::to_string(&cleaned).unwrap_or_default();

    let original_size = raw_json.len();
    let new_size = cleaned_json.len();

    // Count what was removed/truncated.
    let mut thinking_blocks_removed: usize = 0;
    let mut tool_results_truncated: usize = 0;
    let mut bytes_to_remove: usize = 0;
    let mut bytes_to_truncate: usize = 0;

    let protected = compute_protected_indices(&thread.messages, config.keep_last_n_dialogs);
    let dup_ids = compute_duplicate_read_file_ids(&thread.messages, &protected);
    let mut duplicate_read_file_removed: usize = 0;

    for (idx, msg) in thread.messages.iter().enumerate() {
        if protected.contains(&idx) {
            continue;
        }
        if let Message::Agent { agent } = msg {
            for content in &agent.content {
                match content {
                    AgentContent::Thinking(t) => {
                        thinking_blocks_removed += 1;
                        bytes_to_remove +=
                            t.text.len() + t.signature.as_ref().map_or(0, |s| s.len());
                    }
                    AgentContent::RedactedThinking(r) => {
                        thinking_blocks_removed += 1;
                        bytes_to_remove += r.data.as_ref().map_or(0, |d| d.len());
                    }
                    _ => {}
                }
            }

            if let Some(ref reasoning) = agent.reasoning_details {
                bytes_to_remove += reasoning.to_string().len();
            }

            if let Some(ref tr) = agent.tool_results {
                for result in tr.values() {
                    if dup_ids.contains(&result.tool_use_id) {
                        duplicate_read_file_removed += 1;
                        if let Some(ref c) = result.content {
                            bytes_to_remove += content_value_len(c);
                        }
                        continue;
                    }
                    if let Some(limit) = tool_byte_limit(&result.tool_name, config) {
                        if let Some(ref c) = result.content {
                            let clen = content_value_len(c);
                            if clen > limit {
                                tool_results_truncated += 1;
                                bytes_to_truncate += clen - limit;
                            }
                        }
                    }
                }
            }
        }
    }

    let reduction = if original_size > 0 {
        ((original_size as f64 - new_size as f64) / original_size as f64 * 100.0) as f32
    } else {
        0.0
    };

    CleanupPreview {
        bytes_to_remove,
        bytes_to_truncate,
        thinking_blocks_removed,
        tool_results_truncated,
        duplicate_read_file_removed,
        estimated_new_size: new_size,
        reduction_percent: reduction,
    }
}

// ---------------------------------------------------------------------------
// Stats
// ---------------------------------------------------------------------------

pub fn compute_stats(thread: &DbThread, raw_json: &str, compressed_size: usize) -> ThreadStats {
    let mut stats = ThreadStats {
        compressed_size,
        uncompressed_size: raw_json.len(),
        ..Default::default()
    };

    let mut tool_counts: HashMap<String, usize> = HashMap::new();

    for msg in &thread.messages {
        stats.total_messages += 1;
        match msg {
            Message::User { user: u } => {
                stats.user_messages += 1;
                for content in &u.content {
                    match content {
                        crate::types::UserContent::Text { text } => {
                            stats.text_bytes += text.len();
                        }
                        crate::types::UserContent::Other(v) => {
                            stats.text_bytes += v.to_string().len();
                        }
                    }
                }
            }
            Message::Agent { agent } => {
                stats.agent_messages += 1;
                for content in &agent.content {
                    match content {
                        AgentContent::Thinking(t) => {
                            stats.thinking_bytes += t.text.len();
                            stats.thinking_bytes += t.signature.as_ref().map_or(0, |s| s.len());
                        }
                        AgentContent::RedactedThinking(r) => {
                            stats.thinking_bytes += r.data.as_ref().map_or(0, |d| d.len());
                        }
                        AgentContent::Text(t) => {
                            stats.text_bytes += t.len();
                        }
                        AgentContent::ToolUse(tu) => {
                            *tool_counts.entry(tu.name.clone()).or_insert(0) += 1;
                        }
                    }
                }
                if let Some(ref reasoning) = agent.reasoning_details {
                    stats.thinking_bytes += reasoning.to_string().len();
                }
                if let Some(ref tr) = agent.tool_results {
                    for result in tr.values() {
                        if let Some(ref c) = result.content {
                            stats.tool_results_bytes += content_value_len(c);
                        }
                        if let Some(ref o) = result.output {
                            stats.tool_results_bytes += o.to_string().len();
                        }
                    }
                }
            }
            Message::Other(_) => {}
        }
    }

    let mut counts: Vec<(String, usize)> = tool_counts.into_iter().collect();
    counts.sort_by(|a, b| b.1.cmp(&a.1));
    stats.tool_call_counts = counts;

    stats
}

// ---------------------------------------------------------------------------
// Thread analysis (per-tool-category breakdown)
// ---------------------------------------------------------------------------

pub fn analyze_thread(thread: &DbThread, keep_last_n: usize) -> ThreadAnalysis {
    let protected = compute_protected_indices(&thread.messages, keep_last_n);

    let mut cat_map: std::collections::HashMap<String, (usize, usize, usize, usize, usize)> =
        std::collections::HashMap::new();
    // map: tool_name -> (count, total_bytes, max_bytes, in_protected_count, cleanable_bytes)

    let mut total_tool_result_bytes: usize = 0;
    let mut total_agent_text_bytes: usize = 0;
    let mut total_tool_use_bytes: usize = 0;
    let mut total_user_bytes: usize = 0;
    let mut message_count: usize = 0;

    for (idx, msg) in thread.messages.iter().enumerate() {
        message_count += 1;
        match msg {
            Message::User { user: u } => {
                for content in &u.content {
                    match content {
                        crate::types::UserContent::Text { text } => {
                            total_user_bytes += text.len();
                        }
                        crate::types::UserContent::Other(v) => {
                            total_user_bytes += v.to_string().len();
                        }
                    }
                }
            }
            Message::Agent { agent } => {
                let is_protected = protected.contains(&idx);

                for content in &agent.content {
                    match content {
                        AgentContent::Text(t) => {
                            total_agent_text_bytes += t.len();
                        }
                        AgentContent::ToolUse(tu) => {
                            total_tool_use_bytes += tu.raw_input.as_ref().map_or(0, |s| s.len());
                        }
                        _ => {}
                    }
                }

                if let Some(ref tr) = agent.tool_results {
                    for result in tr.values() {
                        let bytes = result
                            .content
                            .as_ref()
                            .map(|c| {
                                extract_content_text(c).map(|s| s.len()).unwrap_or_else(|| {
                                    serde_json::to_string(c).unwrap_or_default().len()
                                })
                            })
                            .unwrap_or(0);
                        total_tool_result_bytes += bytes;

                        let entry = cat_map
                            .entry(result.tool_name.clone())
                            .or_insert((0, 0, 0, 0, 0));
                        entry.0 += 1; // count
                        entry.1 += bytes; // total_bytes
                        if bytes > entry.2 {
                            entry.2 = bytes; // max_bytes
                        }
                        if is_protected {
                            entry.3 += 1; // in_protected
                        } else {
                            // Calculate actual savings: bytes saved = original - truncated
                            let limit = tool_byte_limit(&result.tool_name, &CleanConfig::default());
                            let content_savings = match limit {
                                Some(lim) if bytes > lim => bytes - lim,
                                _ => 0,
                            };
                            // Also count output field savings (truncated at limit/2)
                            let output_bytes = result
                                .output
                                .as_ref()
                                .map(|o| serde_json::to_string(o).unwrap_or_default().len())
                                .unwrap_or(0);
                            let output_savings = match limit {
                                Some(lim) if output_bytes > lim / 2 => output_bytes - lim / 2,
                                _ => 0,
                            };
                            entry.4 += content_savings + output_savings; // cleanable_bytes = actual savings
                        }
                    }
                }
            }
            Message::Other(_) => {}
        }
    }

    let mut categories: Vec<ToolCategoryStats> = cat_map
        .into_iter()
        .map(
            |(name, (count, total_bytes, max_bytes, in_protected, cleanable_bytes))| {
                ToolCategoryStats {
                    tool_name: name,
                    count,
                    total_bytes,
                    max_bytes,
                    in_protected,
                    cleanable_bytes,
                }
            },
        )
        .collect();
    categories.sort_by(|a, b| b.total_bytes.cmp(&a.total_bytes));

    let grand_total_bytes =
        total_tool_result_bytes + total_agent_text_bytes + total_tool_use_bytes + total_user_bytes;

    ThreadAnalysis {
        categories,
        total_tool_result_bytes,
        total_agent_text_bytes,
        total_tool_use_bytes,
        total_user_bytes,
        grand_total_bytes,
        message_count,
        protected_message_count: protected.len(),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;
    use serde_json::json;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn make_user(id: &str) -> Message {
        Message::User {
            user: UserMessage {
                id: id.to_string(),
                content: vec![UserContent::Text {
                    text: "hello".to_string(),
                }],
            },
        }
    }

    fn make_agent_with_thinking(thinking_text: &str) -> Message {
        Message::Agent {
            agent: AgentMessage {
                content: vec![
                    AgentContent::Thinking(ThinkingBlock {
                        text: thinking_text.to_string(),
                        signature: Some("sig".to_string()),
                    }),
                    AgentContent::Text("response".to_string()),
                ],
                tool_results: None,
                reasoning_details: Some(json!({"thinking": true})),
            },
        }
    }

    fn make_agent_with_tool_result(tool_name: &str, content: &str) -> Message {
        let tool_use_id = format!("tu_{}", tool_name);
        let mut tr = HashMap::new();
        tr.insert(
            tool_use_id.clone(),
            ToolResult {
                tool_use_id: tool_use_id.clone(),
                tool_name: tool_name.to_string(),
                is_error: false,
                content: Some(Value::String(content.to_string())),
                output: None,
            },
        );
        Message::Agent {
            agent: AgentMessage {
                content: vec![
                    AgentContent::Text("I'll run a tool".to_string()),
                    AgentContent::ToolUse(ToolUseBlock {
                        id: tool_use_id,
                        name: tool_name.to_string(),
                        raw_input: None,
                        input: None,
                        is_input_complete: Some(true),
                        thought_signature: None,
                    }),
                ],
                tool_results: Some(tr),
                reasoning_details: None,
            },
        }
    }

    fn make_agent_with_read_file(tool_use_id: &str, path: &str, content: &str) -> Message {
        let mut tr = HashMap::new();
        tr.insert(
            tool_use_id.to_string(),
            ToolResult {
                tool_use_id: tool_use_id.to_string(),
                tool_name: "read_file".to_string(),
                is_error: false,
                content: Some(Value::String(content.to_string())),
                output: None,
            },
        );
        Message::Agent {
            agent: AgentMessage {
                content: vec![
                    AgentContent::Text("reading file".to_string()),
                    AgentContent::ToolUse(ToolUseBlock {
                        id: tool_use_id.to_string(),
                        name: "read_file".to_string(),
                        raw_input: None,
                        input: Some(json!({"path": path})),
                        is_input_complete: Some(true),
                        thought_signature: None,
                    }),
                ],
                tool_results: Some(tr),
                reasoning_details: None,
            },
        }
    }

    fn make_thread(messages: Vec<Message>) -> DbThread {
        let mut request_token_usage = HashMap::new();
        for msg in &messages {
            if let Message::User { user: u } = msg {
                request_token_usage.insert(
                    u.id.clone(),
                    TokenUsage {
                        input_tokens: Some(100),
                        output_tokens: Some(50),
                        cache_creation_input_tokens: Some(10),
                        cache_read_input_tokens: Some(5),
                    },
                );
            }
        }
        DbThread {
            title: Some("Test Thread".to_string()),
            version: Some("1".to_string()),
            updated_at: Some("2025-01-01".to_string()),
            messages,
            detailed_summary: None,
            initial_project_snapshot: Some(json!({"files": ["a.rs", "b.rs"]})),
            cumulative_token_usage: Some(json!({"input_tokens": 200})),
            request_token_usage: Some(request_token_usage),
            model: None,
            profile: None,
            subagent_context: None,
            imported: None,
            speed: None,
            thinking_enabled: None,
            thinking_effort: None,
            draft_prompt: None,
            ui_scroll_position: None,
        }
    }

    fn config_keep_0() -> CleanConfig {
        CleanConfig {
            keep_last_n_dialogs: 0,
            ..Default::default()
        }
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_thinking_removed() {
        let thread = make_thread(vec![
            make_user("u1"),
            make_agent_with_thinking("deep thoughts"),
        ]);
        let cleaned = clean_thread(&thread, &config_keep_0());

        if let Message::Agent { agent } = &cleaned.messages[1] {
            for c in &agent.content {
                assert!(
                    !matches!(c, AgentContent::Thinking(_)),
                    "Thinking should be removed"
                );
                assert!(
                    !matches!(c, AgentContent::RedactedThinking(_)),
                    "RedactedThinking should be removed"
                );
            }
            // Text should remain.
            assert!(agent
                .content
                .iter()
                .any(|c| matches!(c, AgentContent::Text(_))));
        } else {
            panic!("Expected Agent message");
        }
    }

    #[test]
    fn test_thinking_preserved_in_protected() {
        let thread = make_thread(vec![
            make_user("u1"),
            make_agent_with_thinking("deep thoughts"),
        ]);
        let config = CleanConfig {
            keep_last_n_dialogs: 10,
            ..Default::default()
        };
        let cleaned = clean_thread(&thread, &config);

        if let Message::Agent { agent } = &cleaned.messages[1] {
            assert!(
                agent
                    .content
                    .iter()
                    .any(|c| matches!(c, AgentContent::Thinking(_))),
                "Thinking should be preserved in protected messages"
            );
        } else {
            panic!("Expected Agent message");
        }
    }

    #[test]
    fn test_tool_result_truncated() {
        let long_content = "x".repeat(5000);
        let thread = make_thread(vec![
            make_user("u1"),
            make_agent_with_tool_result("terminal", &long_content),
        ]);
        let cleaned = clean_thread(&thread, &config_keep_0());

        if let Message::Agent { agent } = &cleaned.messages[1] {
            let tr = agent.tool_results.as_ref().unwrap();
            let result = tr.values().next().unwrap();
            let content_str = result.content.as_ref().unwrap().as_str().unwrap();
            assert!(
                content_str.len() < long_content.len(),
                "Content should be truncated"
            );
            assert!(
                content_str.contains("[TRUNCATED:"),
                "Should contain truncation marker"
            );
        } else {
            panic!("Expected Agent message");
        }
    }

    #[test]
    fn test_preserve_tools_not_truncated() {
        let long_content = "x".repeat(50000);
        let thread = make_thread(vec![
            make_user("u1"),
            make_agent_with_tool_result("create_directory", &long_content),
        ]);
        let cleaned = clean_thread(&thread, &config_keep_0());

        if let Message::Agent { agent } = &cleaned.messages[1] {
            let tr = agent.tool_results.as_ref().unwrap();
            let result = tr.values().next().unwrap();
            let content_str = result.content.as_ref().unwrap().as_str().unwrap();
            assert_eq!(
                content_str.len(),
                long_content.len(),
                "Preserve tools should not be truncated"
            );
        } else {
            panic!("Expected Agent message");
        }
    }

    #[test]
    fn test_duplicate_read_file() {
        let thread = make_thread(vec![
            make_user("u1"),
            make_agent_with_read_file("rf1", "src/main.rs", "first read content"),
            make_user("u2"),
            make_agent_with_read_file("rf2", "src/main.rs", "second read content"),
            make_user("u3"),
            make_agent_with_read_file("rf3", "src/main.rs", "third read content"),
        ]);
        let cleaned = clean_thread(&thread, &config_keep_0());

        // rf1 and rf2 should be deduplicated, rf3 should be kept.
        if let Message::Agent { agent } = &cleaned.messages[1] {
            let tr = agent.tool_results.as_ref().unwrap();
            let result = tr.get("rf1").unwrap();
            assert!(
                result
                    .content
                    .as_ref()
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .contains("[DUPLICATE READ_FILE RESULT REMOVED"),
                "First duplicate should be replaced"
            );
        }

        if let Message::Agent { agent } = &cleaned.messages[3] {
            let tr = agent.tool_results.as_ref().unwrap();
            let result = tr.get("rf2").unwrap();
            assert!(
                result
                    .content
                    .as_ref()
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .contains("[DUPLICATE READ_FILE RESULT REMOVED"),
                "Second duplicate should be replaced"
            );
        }

        if let Message::Agent { agent } = &cleaned.messages[5] {
            let tr = agent.tool_results.as_ref().unwrap();
            let result = tr.get("rf3").unwrap();
            assert_eq!(
                result.content.as_ref().unwrap().as_str().unwrap(),
                "third read content",
                "Last read_file should be preserved"
            );
        }
    }

    #[test]
    fn test_token_usage_recalculated() {
        let mut request_usage = HashMap::new();
        request_usage.insert(
            "u1".to_string(),
            TokenUsage {
                input_tokens: Some(100),
                output_tokens: Some(50),
                cache_creation_input_tokens: Some(10),
                cache_read_input_tokens: Some(5),
            },
        );
        request_usage.insert(
            "u2".to_string(),
            TokenUsage {
                input_tokens: Some(200),
                output_tokens: Some(75),
                cache_creation_input_tokens: None,
                cache_read_input_tokens: Some(15),
            },
        );

        let cumulative = recalculate_cumulative(&request_usage);
        assert_eq!(cumulative["input_tokens"], 300);
        assert_eq!(cumulative["output_tokens"], 125);
        assert_eq!(cumulative["cache_creation_input_tokens"], 10);
        assert_eq!(cumulative["cache_read_input_tokens"], 20);
    }

    #[test]
    fn test_initial_snapshot_nulled() {
        let thread = make_thread(vec![make_user("u1"), make_agent_with_thinking("think")]);
        assert!(thread.initial_project_snapshot.is_some());

        let cleaned = clean_thread(&thread, &config_keep_0());
        assert!(
            cleaned.initial_project_snapshot.is_none(),
            "initial_project_snapshot should be set to None"
        );
    }

    #[test]
    fn test_reasoning_details_removed() {
        let thread = make_thread(vec![make_user("u1"), make_agent_with_thinking("think")]);

        // Verify original has reasoning_details.
        if let Message::Agent { agent } = &thread.messages[1] {
            assert!(agent.reasoning_details.is_some());
        }

        let cleaned = clean_thread(&thread, &config_keep_0());

        if let Message::Agent { agent } = &cleaned.messages[1] {
            assert!(
                agent.reasoning_details.is_none(),
                "reasoning_details should be removed from non-protected messages"
            );
        } else {
            panic!("Expected Agent message");
        }
    }
}
