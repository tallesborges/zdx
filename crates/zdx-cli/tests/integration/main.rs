//! Single-binary harness for `zdx-cli` integration tests.
//!
//! All integration tests are aggregated here so cargo builds and links one
//! test binary instead of one per file. This dramatically cuts compile time
//! after edits to `zdx-cli` or its dependencies.

mod fixtures;

mod cli_help;
mod config_path;
mod login_logout;
mod thread_schema;
mod threads_export;
mod threads_list_show;
mod tool_bash;
mod tool_use_loop;
