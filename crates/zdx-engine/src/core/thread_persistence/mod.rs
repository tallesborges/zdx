//! Thread persistence for ZDX.
//!
//! Each thread is stored as a JSONL file where each line is a JSON object
//! representing an event. Threads use schema versioning (§8 of SPEC).
//!
//! ## Schema v1 Format
//!
//! ```jsonl
//! { "type": "meta", "schema_version": 1, "ts": "2025-12-17T03:21:09Z" }
//! { "type": "message", "role": "user", "text": "...", "ts": "..." }
//! { "type": "tool_use", "id": "...", "name": "read", "input": { "file_path": "..." }, "ts": "..." }
//! { "type": "tool_result", "tool_use_id": "...", "output": { ... }, "ok": true, "ts": "..." }
//! { "type": "reasoning", "text": "...", "replay": { "provider": "openai", "id": "...", "encrypted_content": "..." }, "ts": "..." }
//! { "type": "message", "role": "assistant", "text": "...", "ts": "..." }
//! ```

mod event;
mod format;
mod persist;
mod replay;
mod search;
mod storage;

pub use event::*;
pub use format::*;
pub use persist::*;
pub use replay::*;
pub use search::*;
pub use storage::*;

#[cfg(test)]
mod tests;
