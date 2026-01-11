//! Prompt file helpers.

/// Includes a prompt from the top-level `prompts/` directory.
#[macro_export]
macro_rules! prompt_str {
    ($path:literal) => {
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/prompts/", $path))
    };
}
