//! Single-binary harness for `zdx-cli` integration tests.
//!
//! All integration tests are aggregated here so cargo builds and links one
//! test binary instead of one per file. This dramatically cuts compile time
//! after edits to `zdx-cli` or its dependencies.

mod fixtures;

mod ask_media_audio;
mod background_subagent;
mod bot_gateway;
mod cli_help;
mod config_path;
mod custom_provider_api;
mod engine;
mod exec_subagent;
mod git_go;
mod github_copilot;
mod imagine;
mod jobs_go;
mod jobs_run;
mod login_logout;
mod memory_embed;
mod memory_relevance;
mod model_selection;
mod monitor_go;
mod opencode_go_headers;
mod quota;
mod responses_usage;
mod service;
mod stream_stall;
mod subagent_continuation;
mod thread_schema;
mod threads_export;
mod threads_list_show;
mod tool_bash;
mod tool_use_loop;
mod transcribe;
mod turn_failed_notice;
