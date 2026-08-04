//! Hosted text-embeddings client (OpenAI-compatible `/embeddings` API).
//!
//! Separate from the chat/streaming provider clients: embeddings are an
//! explicit, opt-in corpus-upload path used by `zdx memory index --embed` and
//! query-time vector/hybrid retrieval. Batching, budgets, and persistence live
//! in `zdx-engine`; this module only performs the HTTP call.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::json;

/// One embeddings API call over a batch of inputs.
#[derive(Debug)]
pub struct EmbeddingsRequest<'a> {
    /// Base URL including any `/v1` suffix (e.g. `https://api.openai.com/v1`).
    pub base_url: &'a str,
    pub api_key: &'a str,
    pub model: &'a str,
    /// Optional output dimension override (supported by e.g. `text-embedding-3-*`).
    pub dimensions: Option<u32>,
    pub inputs: &'a [String],
}

/// Vectors for one batch, in input order, plus provider-reported usage.
#[derive(Debug)]
pub struct EmbeddingsResponse {
    pub vectors: Vec<Vec<f32>>,
    pub prompt_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ApiResponse {
    data: Vec<ApiEmbedding>,
    #[serde(default)]
    usage: Option<ApiUsage>,
}

#[derive(Debug, Deserialize)]
struct ApiEmbedding {
    index: usize,
    embedding: Vec<f32>,
}

#[derive(Debug, Deserialize)]
struct ApiUsage {
    #[serde(default)]
    prompt_tokens: Option<u64>,
}

/// Embeds a batch of inputs via an OpenAI-compatible `/embeddings` endpoint.
///
/// # Errors
/// Returns an error on transport/API failures, or when the response does not
/// contain exactly one vector per input.
pub async fn embed(request: &EmbeddingsRequest<'_>) -> Result<EmbeddingsResponse> {
    if request.inputs.is_empty() {
        return Ok(EmbeddingsResponse {
            vectors: Vec::new(),
            prompt_tokens: Some(0),
        });
    }

    let url = format!("{}/embeddings", request.base_url.trim_end_matches('/'));
    let mut body = json!({
        "model": request.model,
        "input": request.inputs,
        "encoding_format": "float",
    });
    if let Some(dimensions) = request.dimensions {
        body["dimensions"] = json!(dimensions);
    }

    let response = reqwest::Client::new()
        .post(&url)
        .bearer_auth(request.api_key)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("send embeddings request to {url}"))?;

    let status = response.status();
    let text = response
        .text()
        .await
        .context("read embeddings response body")?;
    if !status.is_success() {
        let snippet: String = text.chars().take(600).collect();
        bail!("embeddings request failed with {status}: {snippet}");
    }

    let parsed: ApiResponse =
        serde_json::from_str(&text).context("parse embeddings response JSON")?;
    if parsed.data.len() != request.inputs.len() {
        bail!(
            "embeddings response returned {} vectors for {} inputs",
            parsed.data.len(),
            request.inputs.len()
        );
    }

    let mut data = parsed.data;
    data.sort_by_key(|item| item.index);
    let expected_dims = data.first().map_or(0, |item| item.embedding.len());
    if expected_dims == 0 {
        bail!("embeddings response contained an empty vector");
    }
    let mut vectors = Vec::with_capacity(data.len());
    for (position, item) in data.into_iter().enumerate() {
        if item.index != position {
            bail!("embeddings response has missing or duplicate indices");
        }
        if item.embedding.len() != expected_dims {
            bail!(
                "embeddings response mixed vector dimensions ({} vs {expected_dims})",
                item.embedding.len()
            );
        }
        vectors.push(item.embedding);
    }

    Ok(EmbeddingsResponse {
        vectors,
        prompt_tokens: parsed.usage.and_then(|usage| usage.prompt_tokens),
    })
}
