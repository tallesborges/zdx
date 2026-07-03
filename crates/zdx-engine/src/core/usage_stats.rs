//! Usage/cost aggregation over saved thread JSONL.
//!
//! Scans every saved thread under `threads_dir()` and sums token usage,
//! grouped per provider and per model, applying the shared `ModelPricing`
//! cost path. This is the data source for `zdx stats` and the monitor Usage
//! tab.
//!
//! Slice 1 is **best-effort / estimated**: usage events do not yet carry a
//! per-request model/provider, so each thread's usage is attributed to the
//! thread's `model_override` (or a supplied default model). This means:
//! mid-thread model switches are mis-attributed, and forked threads (which
//! copy the parent's events) are double-counted. Slice 2 records model +
//! provider on each usage event, and Slice 3 de-duplicates forks.

use std::collections::BTreeMap;

use anyhow::{Context, Result};

use crate::core::thread_persistence::{self, ThreadEvent};
use crate::models::ModelOption;
use crate::providers::{self, ProviderKind};

/// One aggregated row (a provider total, or a single provider+model total).
#[derive(Debug, Clone)]
pub struct UsageRow {
    /// Provider id (e.g. `anthropic`, `claude-cli`).
    pub provider: String,
    /// Bare model id. `None` for provider-level rows.
    pub model: Option<String>,
    /// Number of usage events attributed to this row.
    pub requests: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Billed USD. Zero for subscription providers and rows with no known pricing.
    pub cost_usd: f64,
    /// Provider is subscription-based (flat rate; token cost is not real spend).
    pub subscription: bool,
    /// Pricing was found in the registry for this row.
    pub cost_known: bool,
    /// Any usage in this row was attributed by best-effort fallback rather than
    /// an explicit per-request provider (see the aggregator resolution order).
    pub estimated: bool,
}

impl UsageRow {
    /// Total tokens across all token classes.
    pub fn tokens(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

/// Overall totals across all scanned threads.
#[derive(Debug, Clone, Default)]
pub struct UsageTotals {
    pub requests: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Sum of billed USD (excludes subscription providers and unknown-pricing rows).
    pub billed_usd: f64,
    /// Tokens spent on subscription providers (not billed per-token).
    pub subscription_tokens: u64,
    /// Number of (provider, model) rows with no known pricing.
    pub unknown_pricing_rows: u64,
}

impl UsageTotals {
    pub fn tokens(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

/// Aggregated usage statistics ready for display.
#[derive(Debug, Clone)]
pub struct UsageStats {
    pub totals: UsageTotals,
    /// Per-provider rows, sorted by total tokens (descending).
    pub by_provider: Vec<UsageRow>,
    /// Per provider+model rows, sorted by total tokens (descending).
    pub by_model: Vec<UsageRow>,
    /// Number of thread files successfully scanned.
    pub threads_scanned: usize,
    /// Non-fatal issues (e.g. an unreadable thread file that was skipped).
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct RawBucket {
    requests: u64,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    /// True if any contributing event lacked an explicit provider and had to
    /// be attributed by best-effort fallback (thread `model_override` or bare
    /// model resolution).
    estimated: bool,
}

impl RawBucket {
    fn add_event(
        &mut self,
        input: u64,
        output: u64,
        cache_read: u64,
        cache_write: u64,
        estimated: bool,
    ) {
        self.requests += 1;
        self.input += input;
        self.output += output;
        self.cache_read += cache_read;
        self.cache_write += cache_write;
        self.estimated |= estimated;
    }

    fn merge(&mut self, other: &RawBucket) {
        self.requests += other.requests;
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
        self.estimated |= other.estimated;
    }

    fn tokens(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

/// Aggregate usage across all saved threads.
///
/// `default_model` is used to attribute usage for threads that have no
/// `model_override` (typically the active `config.model`).
///
/// Per-thread read failures are collected into `warnings` and skipped; the
/// scan only fails outright if the threads directory itself cannot be listed.
///
/// # Errors
/// Returns an error if the threads directory cannot be listed.
pub fn aggregate_usage(default_model: &str) -> Result<UsageStats> {
    let threads = thread_persistence::list_threads().context("list threads for usage stats")?;

    let mut raw: BTreeMap<(String, String), RawBucket> = BTreeMap::new();
    let mut warnings = Vec::new();
    let mut threads_scanned = 0usize;

    for summary in &threads {
        let events = match thread_persistence::load_thread_events(&summary.id) {
            Ok(events) => events,
            Err(err) => {
                warnings.push(format!("skipped thread {}: {err}", summary.id));
                continue;
            }
        };
        threads_scanned += 1;

        // Thread-level fallback attribution for events that lack per-request
        // model/provider (old transcripts, pre-Slice-2 usage).
        let fallback_model =
            model_override_from_events(&events).unwrap_or_else(|| default_model.to_string());
        let fallback_selection = providers::resolve_provider(&fallback_model);
        let fallback_key = (
            fallback_selection.kind.id().to_string(),
            fallback_selection.model.clone(),
        );

        for event in &events {
            if let ThreadEvent::Usage {
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
                model,
                provider,
                ..
            } = event
            {
                let (key, estimated) =
                    attribute_event(provider.as_deref(), model.as_deref(), &fallback_key);
                raw.entry(key).or_default().add_event(
                    *input_tokens,
                    *output_tokens,
                    *cache_read_tokens,
                    *cache_write_tokens,
                    estimated,
                );
            }
        }
    }

    Ok(finalize(raw, threads_scanned, warnings))
}

/// Resolves the `(provider, model)` bucket key for a usage event.
///
/// Resolution order: (a) explicit provider on the event → accurate; (b) model
/// present without provider → resolve any `provider:` prefix, else estimated;
/// (c) no attribution → thread-level fallback, estimated.
fn attribute_event(
    provider: Option<&str>,
    model: Option<&str>,
    fallback_key: &(String, String),
) -> ((String, String), bool) {
    let provider = provider.filter(|p| !p.is_empty());
    let model = model.filter(|m| !m.is_empty());
    match (provider, model) {
        (Some(provider), Some(model)) => ((provider.to_string(), model.to_string()), false),
        (Some(provider), None) => ((provider.to_string(), String::new()), true),
        (None, Some(model)) => {
            let selection = providers::resolve_provider(model);
            ((selection.kind.id().to_string(), selection.model), true)
        }
        (None, None) => (fallback_key.clone(), true),
    }
}

fn model_override_from_events(events: &[ThreadEvent]) -> Option<String> {
    events.iter().find_map(|event| match event {
        ThreadEvent::Meta { model_override, .. } => model_override.clone(),
        _ => None,
    })
}

fn finalize(
    raw: BTreeMap<(String, String), RawBucket>,
    threads_scanned: usize,
    warnings: Vec<String>,
) -> UsageStats {
    let mut by_model = Vec::with_capacity(raw.len());
    let mut provider_raw: BTreeMap<String, RawBucket> = BTreeMap::new();
    let mut provider_subscription: BTreeMap<String, bool> = BTreeMap::new();
    let mut provider_cost: BTreeMap<String, f64> = BTreeMap::new();
    let mut provider_unknown: BTreeMap<String, bool> = BTreeMap::new();
    let mut totals = UsageTotals::default();

    for ((provider, model), bucket) in raw {
        let subscription =
            ProviderKind::from_id(&provider).is_some_and(ProviderKind::is_subscription);
        let pricing = ModelOption::find_by_provider_and_id(&provider, &model).map(|m| m.pricing);
        let cost_known = pricing.is_some();
        let cost_usd = if subscription {
            0.0
        } else {
            pricing.map_or(0.0, |p| {
                p.cost(
                    bucket.input,
                    bucket.output,
                    bucket.cache_read,
                    bucket.cache_write,
                )
            })
        };

        totals.requests += bucket.requests;
        totals.input += bucket.input;
        totals.output += bucket.output;
        totals.cache_read += bucket.cache_read;
        totals.cache_write += bucket.cache_write;
        if subscription {
            totals.subscription_tokens += bucket.tokens();
        } else if cost_known {
            totals.billed_usd += cost_usd;
        } else {
            totals.unknown_pricing_rows += 1;
        }

        provider_raw
            .entry(provider.clone())
            .or_default()
            .merge(&bucket);
        provider_subscription.insert(provider.clone(), subscription);
        *provider_cost.entry(provider.clone()).or_insert(0.0) += cost_usd;
        let unknown = !subscription && !cost_known;
        let entry = provider_unknown.entry(provider.clone()).or_insert(false);
        *entry = *entry || unknown;

        by_model.push(UsageRow {
            provider,
            model: Some(model),
            requests: bucket.requests,
            input: bucket.input,
            output: bucket.output,
            cache_read: bucket.cache_read,
            cache_write: bucket.cache_write,
            cost_usd,
            subscription,
            cost_known,
            estimated: bucket.estimated,
        });
    }

    let mut by_provider: Vec<UsageRow> = provider_raw
        .into_iter()
        .map(|(provider, bucket)| {
            let subscription = provider_subscription
                .get(&provider)
                .copied()
                .unwrap_or(false);
            let has_unknown = provider_unknown.get(&provider).copied().unwrap_or(false);
            UsageRow {
                cost_usd: provider_cost.get(&provider).copied().unwrap_or(0.0),
                requests: bucket.requests,
                input: bucket.input,
                output: bucket.output,
                cache_read: bucket.cache_read,
                cache_write: bucket.cache_write,
                subscription,
                cost_known: !has_unknown,
                estimated: bucket.estimated,
                provider,
                model: None,
            }
        })
        .collect();

    by_provider.sort_by(|a, b| {
        b.tokens()
            .cmp(&a.tokens())
            .then(a.provider.cmp(&b.provider))
    });
    by_model.sort_by(|a, b| {
        b.tokens()
            .cmp(&a.tokens())
            .then(a.provider.cmp(&b.provider))
    });

    UsageStats {
        totals,
        by_provider,
        by_model,
        threads_scanned,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::float_cmp)]
    fn unknown_provider_and_model_is_bucketed_without_cost() {
        let mut raw = BTreeMap::new();
        raw.insert(
            (
                "faketest-provider".to_string(),
                "faketest-model".to_string(),
            ),
            RawBucket {
                requests: 2,
                input: 1_000,
                output: 500,
                cache_read: 0,
                cache_write: 0,
                estimated: false,
            },
        );

        let stats = finalize(raw, 1, Vec::new());

        assert_eq!(stats.by_model.len(), 1);
        let row = &stats.by_model[0];
        assert!(!row.cost_known);
        assert_eq!(row.cost_usd, 0.0);
        assert_eq!(stats.totals.unknown_pricing_rows, 1);
        assert_eq!(stats.totals.requests, 2);
        assert_eq!(stats.totals.tokens(), 1_500);
        assert_eq!(stats.totals.billed_usd, 0.0);
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn subscription_provider_tokens_excluded_from_billed() {
        let Some(sub_provider) = ProviderKind::all()
            .iter()
            .find(|k| k.is_subscription())
            .map(|k| k.id().to_string())
        else {
            return; // no subscription providers registered; nothing to assert
        };

        let mut raw = BTreeMap::new();
        raw.insert(
            (sub_provider.clone(), "any-model".to_string()),
            RawBucket {
                requests: 3,
                input: 2_000,
                output: 1_000,
                cache_read: 500,
                cache_write: 0,
                estimated: false,
            },
        );

        let stats = finalize(raw, 1, Vec::new());

        let row = &stats.by_model[0];
        assert!(row.subscription);
        assert_eq!(row.cost_usd, 0.0);
        assert_eq!(stats.totals.subscription_tokens, 3_500);
        assert_eq!(stats.totals.billed_usd, 0.0);
        assert_eq!(stats.totals.unknown_pricing_rows, 0);
    }
}
