//! `zdx quota` — live subscription-quota snapshot for OAuth providers.

use anyhow::Result;
use zdx_engine::providers::subscription_quota::{
    self, QuotaError, QuotaWindow, SubscriptionQuota, account_display,
};

/// One provider account's fetched result.
struct ProviderResult {
    id: &'static str,
    account: Option<String>,
    quota: std::result::Result<SubscriptionQuota, QuotaError>,
}

/// Runs `zdx quota`, fetching each configured subscription's live quota.
///
/// # Errors
/// Returns an error only if output serialization fails; provider fetch failures
/// are reported per-provider, not as a hard error.
pub async fn run(json: bool) -> Result<()> {
    let mut results = Vec::new();
    for (id, account, fetch) in subscription_quota::stored_accounts()? {
        results.push(ProviderResult {
            quota: fetch(account.clone()).await,
            id,
            account,
        });
    }

    if json {
        print_json(&results)?;
    } else {
        print_text(&results);
    }
    Ok(())
}

fn print_json(results: &[ProviderResult]) -> Result<()> {
    let providers: Vec<serde_json::Value> = results
        .iter()
        .map(|r| match &r.quota {
            Ok(quota) => serde_json::json!({
                "provider": r.id,
                "account": r.account,
                "plan": quota.plan,
                "windows": quota.windows.iter().map(window_json).collect::<Vec<_>>(),
            }),
            Err(err) => serde_json::json!({
                "provider": r.id,
                "account": r.account,
                "error": err.reason(),
            }),
        })
        .collect();
    let out = serde_json::json!({ "providers": providers });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

fn window_json(w: &QuotaWindow) -> serde_json::Value {
    serde_json::json!({
        "label": w.label,
        "used_percent": w.used_percent,
        "resets_at": w.resets_at.map(|dt| dt.to_rfc3339()),
        "scope": w.scope,
    })
}

fn print_text(results: &[ProviderResult]) {
    println!("Subscriptions (live quota)");
    for r in results {
        let name = account_display(r.id, r.account.as_deref());
        match &r.quota {
            Ok(quota) => {
                let plan = quota
                    .plan
                    .as_deref()
                    .map(|p| format!(" [{p}]"))
                    .unwrap_or_default();
                println!("  {name}{plan}");
                for w in &quota.windows {
                    let reset = w
                        .resets_at
                        .map(|dt| format!("   {}", format_reset_in(dt)))
                        .unwrap_or_default();
                    let scope = w
                        .scope
                        .as_deref()
                        .map(|s| format!("   · {s}"))
                        .unwrap_or_default();
                    println!("    {:<7} {:>4.0}%{reset}{scope}", w.label, w.used_percent);
                }
            }
            Err(err) => println!("  {name}   {}", err.reason()),
        }
    }
}

/// Formats a future reset instant as `resets in Xh Ym` (mirrors the monitor).
fn format_reset_in(dt: chrono::DateTime<chrono::Utc>) -> String {
    let secs = (dt - chrono::Utc::now()).num_seconds();
    if secs <= 0 {
        return "reset due".to_string();
    }
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let mins = (secs % 3_600) / 60;
    if days > 0 {
        format!("resets in {days}d {hours}h")
    } else if hours > 0 {
        format!("resets in {hours}h {mins}m")
    } else {
        format!("resets in {mins}m")
    }
}
