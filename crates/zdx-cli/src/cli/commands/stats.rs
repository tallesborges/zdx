//! `zdx stats` — recorded usage across saved threads, split by provider,
//! model, project, and run kind.

use anyhow::{Context, Result};
use zdx_engine::config;
use zdx_engine::core::usage_report::{self, ProjectUsage, RunKind, RunUsage, UsageReport};
use zdx_engine::core::usage_stats::{UsageRow, UsageStats};

/// Runs `zdx stats`: the shared usage report for `days` (all time when
/// `None`), as text or JSON.
pub fn run(config: &config::Config, days: Option<u32>, json: bool) -> Result<()> {
    let report = usage_report::report(&config.model, days).context("aggregate usage stats")?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_report(&report);
    }
    Ok(())
}

fn print_report(report: &UsageReport) {
    let stats = &report.stats;
    let window = report.days.map_or_else(
        || "all time".to_string(),
        |days| format!("last {days} days (UTC)"),
    );
    println!("zdx usage stats (estimated) · {window}");
    println!("Global across all ZDX threads under $ZDX_HOME/threads.");
    println!(
        "Tokens are what ZDX recorded, not provider quota. Estimated: old usage lacks \
         per-request model/provider; includes saved subagent/helper runs; image spend \
         excluded; subscription providers shown as flat-rate."
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

    print_breakdown(report);
    print_warnings(stats);
}

fn print_breakdown(report: &UsageReport) {
    let total = report.stats.totals.tokens();
    println!();
    println!("By project (subagent and helper runs count toward the thread that started them):");
    println!(
        "  {:<52} {:>8} {:>8} {:>10} {:>6}",
        "PROJECT", "THREADS", "REQ", "TOKENS", "SHARE"
    );
    for row in &report.by_project {
        println!(
            "  {:<52} {:>8} {:>8} {:>10} {:>6}",
            truncate_start(&project_label(row), 52),
            row.threads,
            row.counts.requests,
            format_tokens(row.tokens),
            share(row.tokens, total),
        );
    }

    println!();
    println!("By run kind:");
    println!(
        "  {:<40} {:>8} {:>8} {:>10} {:>6}",
        "RUN", "THREADS", "REQ", "TOKENS", "SHARE"
    );
    for row in &report.by_run {
        println!(
            "  {:<40} {:>8} {:>8} {:>10} {:>6}",
            truncate(&run_label(row), 40),
            row.threads,
            row.counts.requests,
            format_tokens(row.tokens),
            share(row.tokens, total),
        );
    }
    if report.unindexed_threads > 0 {
        println!(
            "\n{} thread(s) are not in the thread index; their project and run kind are unknown.",
            report.unindexed_threads
        );
    }
}

fn project_label(row: &ProjectUsage) -> String {
    row.project
        .clone()
        .unwrap_or_else(|| "(unknown)".to_string())
}

fn run_label(row: &RunUsage) -> String {
    let kind = match row.kind {
        RunKind::Agent => "agent",
        RunKind::Profile => "profile",
        RunKind::Worker => "worker",
        RunKind::Subagent => "subagent",
        RunKind::Helper => "helper",
        RunKind::Other => "other",
        RunKind::Unknown => "(unknown)",
    };
    match &row.name {
        Some(name) => format!("{kind}: {name}"),
        None => kind.to_string(),
    }
}

fn share(part: u64, total: u64) -> String {
    if total == 0 {
        return "-".to_string();
    }
    format!("{:.0}%", part as f64 * 100.0 / total as f64)
}

/// Keeps the end of a path, which is the part that names the project.
fn truncate_start(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        text.to_string()
    } else {
        let tail: String = text.chars().skip(count - max.saturating_sub(1)).collect();
        format!("…{tail}")
    }
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

pub(crate) fn format_cost(cost: f64) -> String {
    format!("${cost:.2}")
}

pub(crate) fn format_tokens(count: u64) -> String {
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
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let head: String = text.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

#[cfg(test)]
mod tests {
    use super::truncate;

    #[test]
    fn truncate_cuts_on_character_boundaries() {
        let name = "é".repeat(41);
        let cut = truncate(&name, 40);
        assert_eq!(cut.chars().count(), 40);
        assert!(cut.ends_with('…'));
        assert_eq!(truncate("short", 40), "short");
    }
}
