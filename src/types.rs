use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Meta-info about a thread loaded from DB (without deserializing data).
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ThreadMeta {
    pub id: String,
    pub summary: String,
    pub updated_at: String,
    pub created_at: String,
    pub data_type: String,
    pub data_size: usize,
    /// "Agent", "Chat", or "Unknown"
    pub thread_type: String,
}

/// Root structure of a thread (the data field after decompression).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbThread {
    pub title: Option<String>,
    pub version: Option<String>,
    pub updated_at: Option<String>,
    pub messages: Vec<Message>,
    pub detailed_summary: Option<Value>,
    pub initial_project_snapshot: Option<Value>,
    pub cumulative_token_usage: Option<Value>,
    pub request_token_usage: Option<HashMap<String, TokenUsage>>,
    pub model: Option<Value>,
    pub profile: Option<Value>,
    /// If non-null, this thread is a subagent spawned from another thread.
    #[serde(default)]
    pub subagent_context: Option<Value>,
    // Extra fields Zed stores that we must round-trip unchanged
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_enabled: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_effort: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_prompt: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_scroll_position: Option<Value>,
}

/// A single message in the thread: either User or Agent.
///
/// Zed serializes messages as externally-tagged JSON objects, e.g.
/// User: {"User": {"id": "...", "content": [...]}}
/// Agent: {"Agent": {"content": [...], ...}}
///
/// Serde default (externally-tagged) representation matches this exactly.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Message {
    User {
        #[serde(rename = "User")]
        user: UserMessage,
    },
    Agent {
        #[serde(rename = "Agent")]
        agent: AgentMessage,
    },
    Other(Value),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessage {
    pub id: String,
    pub content: Vec<UserContent>,
}

/// User content is either {"Text": "..."} or some other JSON value we preserve.
///
/// We use serde(untagged) here because we need a fallback Other(Value)
/// variant to capture any unknown content types without losing data.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    Text {
        #[serde(rename = "Text")]
        text: String,
    },
    Other(Value),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    pub content: Vec<AgentContent>,
    pub tool_results: Option<HashMap<String, ToolResult>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_details: Option<Value>,
}

/// Agent content blocks.
///
/// Zed serializes these as externally-tagged JSON objects, e.g.
/// {"Thinking": {"text": "...", "signature": "..."}}
/// {"RedactedThinking": {"data": "..."}}
/// {"Text": "..."}
/// {"ToolUse": {"id": "...", "name": "...", ...}}
///
/// Serde default (externally-tagged) representation matches this exactly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentContent {
    Thinking(ThinkingBlock),
    RedactedThinking(Value),
    Text(String),
    ToolUse(ToolUseBlock),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingBlock {
    pub text: String,
    pub signature: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUseBlock {
    pub id: String,
    pub name: String,
    pub raw_input: Option<String>,
    pub input: Option<Value>,
    pub is_input_complete: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub tool_name: String,
    pub is_error: bool,
    pub content: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
}

/// Statistics displayed in the right panel.
#[derive(Debug, Clone, Default)]
pub struct ThreadStats {
    pub total_messages: usize,
    pub user_messages: usize,
    pub agent_messages: usize,
    pub compressed_size: usize,
    pub uncompressed_size: usize,
    pub thinking_bytes: usize,
    pub tool_results_bytes: usize,
    pub text_bytes: usize,
    pub tool_call_counts: Vec<(String, usize)>,
}

/// Cleanup preview statistics.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct CleanupPreview {
    pub bytes_to_remove: usize,
    pub bytes_to_truncate: usize,
    pub thinking_blocks_removed: usize,
    pub tool_results_truncated: usize,
    pub duplicate_read_file_removed: usize,
    pub estimated_new_size: usize,
    pub reduction_percent: f32,
}

/// Per-tool analysis entry - one row in the category breakdown table.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ToolCategoryStats {
    pub tool_name: String,
    pub count: usize,
    pub total_bytes: usize,
    pub max_bytes: usize,
    /// Number of results inside the protected (keep_last_n) zone.
    pub in_protected: usize,
    /// Bytes outside the protected zone (actually cleanable).
    pub cleanable_bytes: usize,
}

/// Full detailed analysis of a thread, computed by cleaner::analyze_thread.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct ThreadAnalysis {
    /// Per-tool-name stats, sorted by total_bytes descending.
    pub categories: Vec<ToolCategoryStats>,
    pub total_tool_result_bytes: usize,
    pub total_agent_text_bytes: usize,
    pub total_tool_use_bytes: usize,
    pub total_user_bytes: usize,
    pub grand_total_bytes: usize,
    pub message_count: usize,
    pub protected_message_count: usize,
}
