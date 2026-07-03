//! `zdx stats` — usage/cost summary across saved threads.

use anyhow::{Context, Result};
use zdx_engine::config;
use zdx_engine::core::usage_stats::{self, UsageRow, UsageStats};

/// Runs `zdx stats`, printing a usage/cost breakdown per provider and model.
pub fn run(config: &config::Config) -> Result<()> {
    let stats = usage_stats::aggregate_usage(&config.model).context("aggregate usage stats")?;
    print_stats(&stats);
    Ok(())
}

fn print_stats(stats: &UsageStats) {
    println!("zdx usage stats (estimated)");
    println!("Global across all ZDX threads under $ZDX_HOME/threads.");
    println!(
        "Estimated: old usage lacks per-request model/provider; subagent/helper + image \
         spend excluded; subscription providers shown as flat-rate."
    );
    println!();

    if stats.threads_scanned == 0 || stats.totals.requests == 0 {
        println!("No usage found in {} thread(s).", stats.threads_scanned);
        print_warnings(stats);
        return;
    }

    let t = &stats.totals;
    println!(
        "Overall: {} requests · {} tokens (in {} / out {} / cache-r {} / cache-w {})",
        t.requests,
        format_tokens(t.tokens()),
        format_tokens(t.input),
        format_tokens(t.output),
        format_tokens(t.cache_read),
        format_tokens(t.cache_write),
    );
    println!(
        "Billed: {}   Subscription tokens: {}   Unknown-pricing rows: {}",
        format_cost(t.billed_usd),
        format_tokens(t.subscription_tokens),
        t.unknown_pricing_rows,
    );
    println!("Scanned {} thread(s).", stats.threads_scanned);

    println!();
    println!("By provider:");
    println!(
        "  {:<16} {:>8} {:>10} {:>14}",
        "PROVIDER", "REQ", "TOKENS", "COST"
    );
    for row in &stats.by_provider {
        println!(
            "  {:<16} {:>8} {:>10} {:>14}",
            truncate(&row.provider, 16),
            row.requests,
            format_tokens(row.tokens()),
            cost_cell(row),
        );
    }

    println!();
    println!("By model:");
    println!(
        "  {:<34} {:<16} {:>8} {:>10} {:>14}",
        "MODEL", "PROVIDER", "REQ", "TOKENS", "COST"
    );
    for row in &stats.by_model {
        let model = row.model.as_deref().unwrap_or("-");
        println!(
            "  {:<34} {:<16} {:>8} {:>10} {:>14}",
            truncate(model, 34),
            truncate(&row.provider, 16),
            row.requests,
            format_tokens(row.tokens()),
            cost_cell(row),
        );
    }

    if stats.by_model.iter().any(|row| row.estimated) {
        println!(
            "\n* estimated — attributed without a per-request provider (older usage or fallback)."
        );
    }

    print_warnings(stats);
}

fn print_warnings(stats: &UsageStats) {
    if stats.warnings.is_empty() {
        return;
    }
    eprintln!();
    eprintln!("{} thread(s) skipped:", stats.warnings.len());
    for warning in &stats.warnings {
        eprintln!("  - {warning}");
    }
}

fn cost_cell(row: &UsageRow) -> String {
    let base = if row.subscription {
        "subscription".to_string()
    } else if !row.cost_known {
        "unknown".to_string()
    } else {
        format_cost(row.cost_usd)
    };
    if row.estimated {
        format!("{base}*")
    } else {
        base
    }
}

fn format_cost(cost: f64) -> String {
    format!("${cost:.2}")
}

fn format_tokens(count: u64) -> String {
    if count >= 1_000_000_000 {
        format!("{:.1}B", count as f64 / 1_000_000_000.0)
    } else if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        text.to_string()
    } else {
        format!("{}…", &text[..max.saturating_sub(1)])
    }
}
