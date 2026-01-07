//! Codex instruction prompt loading.
//!
//! Prompts are copied verbatim from:
//! https://raw.githubusercontent.com/openai/codex/rust-v0.79.0/codex-rs/core/<prompt_file>
//! Keep files byte-for-byte identical; Codex validates instructions strictly.

const PROMPT_GPT_5_CODEX: &str = include_str!("gpt_5_codex_prompt.md");
const PROMPT_GPT_5_1: &str = include_str!("gpt_5_1_prompt.md");
const PROMPT_GPT_5_2: &str = include_str!("gpt_5_2_prompt.md");
const PROMPT_GPT_5_1_CODEX_MAX: &str = include_str!("gpt-5.1-codex-max_prompt.md");
const PROMPT_GPT_5_2_CODEX: &str = include_str!("gpt-5.2-codex_prompt.md");

const MODEL_MAP: &[(&str, &str)] = &[
    ("gpt-5.1-codex", "gpt-5.1-codex"),
    ("gpt-5.1-codex-low", "gpt-5.1-codex"),
    ("gpt-5.1-codex-medium", "gpt-5.1-codex"),
    ("gpt-5.1-codex-high", "gpt-5.1-codex"),
    ("gpt-5.1-codex-max", "gpt-5.1-codex-max"),
    ("gpt-5.1-codex-max-low", "gpt-5.1-codex-max"),
    ("gpt-5.1-codex-max-medium", "gpt-5.1-codex-max"),
    ("gpt-5.1-codex-max-high", "gpt-5.1-codex-max"),
    ("gpt-5.1-codex-max-xhigh", "gpt-5.1-codex-max"),
    ("gpt-5.2", "gpt-5.2"),
    ("gpt-5.2-none", "gpt-5.2"),
    ("gpt-5.2-low", "gpt-5.2"),
    ("gpt-5.2-medium", "gpt-5.2"),
    ("gpt-5.2-high", "gpt-5.2"),
    ("gpt-5.2-xhigh", "gpt-5.2"),
    ("gpt-5.2-codex", "gpt-5.2-codex"),
    ("gpt-5.2-codex-low", "gpt-5.2-codex"),
    ("gpt-5.2-codex-medium", "gpt-5.2-codex"),
    ("gpt-5.2-codex-high", "gpt-5.2-codex"),
    ("gpt-5.2-codex-xhigh", "gpt-5.2-codex"),
    ("gpt-5.1-codex-mini", "gpt-5.1-codex-mini"),
    ("gpt-5.1-codex-mini-medium", "gpt-5.1-codex-mini"),
    ("gpt-5.1-codex-mini-high", "gpt-5.1-codex-mini"),
    ("gpt-5.1", "gpt-5.1"),
    ("gpt-5.1-none", "gpt-5.1"),
    ("gpt-5.1-low", "gpt-5.1"),
    ("gpt-5.1-medium", "gpt-5.1"),
    ("gpt-5.1-high", "gpt-5.1"),
    ("gpt-5.1-chat-latest", "gpt-5.1"),
    ("gpt-5-codex", "gpt-5.1-codex"),
    ("codex-mini-latest", "gpt-5.1-codex-mini"),
    ("gpt-5-codex-mini", "gpt-5.1-codex-mini"),
    ("gpt-5-codex-mini-medium", "gpt-5.1-codex-mini"),
    ("gpt-5-codex-mini-high", "gpt-5.1-codex-mini"),
    ("gpt-5", "gpt-5.1"),
    ("gpt-5-mini", "gpt-5.1"),
    ("gpt-5-nano", "gpt-5.1"),
];

pub fn get_codex_instructions(model: &str) -> String {
    let normalized = normalize_model(model);
    let prompt = match normalized.as_str() {
        "gpt-5.2-codex" => PROMPT_GPT_5_2_CODEX,
        "gpt-5.1-codex-max" => PROMPT_GPT_5_1_CODEX_MAX,
        "gpt-5.2" => PROMPT_GPT_5_2,
        "gpt-5.1" => PROMPT_GPT_5_1,
        _ => PROMPT_GPT_5_CODEX,
    };

    prompt.to_string()
}

pub fn normalize_model(model: &str) -> String {
    let raw = model.split('/').next_back().unwrap_or(model).to_lowercase();

    if let Some(mapped) = mapped_model(&raw) {
        return mapped.to_string();
    }

    if raw.contains("gpt-5.2-codex") || raw.contains("gpt 5.2 codex") {
        return "gpt-5.2-codex".to_string();
    }
    if raw.contains("gpt-5.2") || raw.contains("gpt 5.2") {
        return "gpt-5.2".to_string();
    }
    if raw.contains("gpt-5.1-codex-max") || raw.contains("gpt 5.1 codex max") {
        return "gpt-5.1-codex-max".to_string();
    }
    if raw.contains("gpt-5.1-codex-mini") || raw.contains("gpt 5.1 codex mini") {
        return "gpt-5.1-codex-mini".to_string();
    }
    if raw.contains("codex-mini-latest")
        || raw.contains("gpt-5-codex-mini")
        || raw.contains("gpt 5 codex mini")
    {
        return "codex-mini-latest".to_string();
    }
    if raw.contains("gpt-5.1-codex") || raw.contains("gpt 5.1 codex") {
        return "gpt-5.1-codex".to_string();
    }
    if raw.contains("gpt-5.1") || raw.contains("gpt 5.1") {
        return "gpt-5.1".to_string();
    }
    if raw.contains("codex") {
        return "gpt-5.1-codex".to_string();
    }
    if raw.contains("gpt-5") || raw.contains("gpt 5") {
        return "gpt-5.1".to_string();
    }

    "gpt-5.1".to_string()
}

fn mapped_model(model: &str) -> Option<&'static str> {
    for (key, value) in MODEL_MAP {
        if *key == model {
            return Some(*value);
        }
    }

    let lower = model.to_lowercase();
    for (key, value) in MODEL_MAP {
        if key.to_lowercase() == lower {
            return Some(*value);
        }
    }

    None
}
