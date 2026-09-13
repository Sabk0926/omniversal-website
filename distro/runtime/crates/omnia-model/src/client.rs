//! Client for the llama.cpp server.
//!
//! Targets the native `/completion` endpoint rather than the OpenAI-compatible
//! one, for a specific reason: it takes `cache_prompt` and reports how many
//! prompt tokens were actually reused. Without that number, KV cache reuse is
//! a setting you hope is working. With it, `omni doctor` can say so.

use std::time::Duration;

use omnia_core::config::ModelConfig;
use omnia_http::{Client as Http, HttpError};
use serde::Deserialize;
use serde_json::json;

use crate::prompt::Prompt;

#[derive(Debug)]
pub enum ModelError {
    /// Nothing is listening. The caller decides whether that is worth saying:
    /// `omni doctor` reports it, the shell integration falls through silently.
    Unavailable(String),
    /// The server answered, but not with something we can use.
    Protocol(String),
    Http(HttpError),
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelError::Unavailable(what) => write!(f, "model server unavailable: {what}"),
            ModelError::Protocol(what) => {
                write!(f, "unexpected response from model server: {what}")
            }
            ModelError::Http(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ModelError {}

impl From<HttpError> for ModelError {
    fn from(e: HttpError) -> Self {
        match e {
            HttpError::Connect { addr, source } => {
                ModelError::Unavailable(format!("{addr}: {source}"))
            }
            other => ModelError::Http(other),
        }
    }
}

pub type Result<T> = std::result::Result<T, ModelError>;

/// What the prefill actually cost, and how much of it was skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CacheStats {
    /// Prompt tokens the server had to evaluate this time.
    pub evaluated: u32,
    /// Prompt tokens served from the KV cache.
    pub cached: u32,
    pub generated: u32,
    pub prefill_ms: u64,
    pub generate_ms: u64,
}

impl CacheStats {
    pub fn prompt_tokens(self) -> u32 {
        self.evaluated + self.cached
    }

    /// Fraction of the prompt served from cache, 0.0 to 1.0.
    pub fn hit_ratio(self) -> f32 {
        let total = self.prompt_tokens();
        if total == 0 {
            return 0.0;
        }
        f32::from(u16::try_from(self.cached).unwrap_or(u16::MAX))
            / f32::from(u16::try_from(total).unwrap_or(u16::MAX))
    }

    /// A one-line summary for `omni doctor` and debug logs.
    pub fn describe(self) -> String {
        format!(
            "{} prompt tokens ({} cached, {:.0}%), {} generated, {} ms prefill, {} ms generate",
            self.prompt_tokens(),
            self.cached,
            self.hit_ratio() * 100.0,
            self.generated,
            self.prefill_ms,
            self.generate_ms,
        )
    }
}

#[derive(Debug, Clone)]
pub struct Completion {
    pub text: String,
    pub stats: CacheStats,
    /// True when generation stopped because it hit the token limit rather than
    /// finishing. Callers that parse structured output must check this: a
    /// truncated JSON plan is not a plan.
    pub truncated: bool,
}

#[derive(Debug, Clone)]
pub struct ModelClient {
    http: Http,
    cache_prompt: bool,
    max_tokens: u32,
    temperature: f32,
}

impl ModelClient {
    pub fn from_config(config: &ModelConfig) -> ModelClient {
        ModelClient {
            http: Http::new(
                config.host.clone(),
                config.port,
                Duration::from_secs(config.request_timeout_seconds),
            ),
            cache_prompt: config.cache_static_prefix,
            max_tokens: config.max_tokens,
            temperature: config.temperature,
        }
    }

    pub fn reachable(&self) -> bool {
        self.http.reachable()
    }

    /// Run a prompt to completion.
    pub fn complete(&self, prompt: &Prompt) -> Result<Completion> {
        // A clock in the cacheable prefix silently costs a full prefill every
        // request. Warn rather than fail: slow is better than refusing.
        for problem in prompt.check_stability() {
            omnia_core::warn!("model", "{problem}");
        }

        let body = json!({
            "prompt": prompt.render(),
            "cache_prompt": self.cache_prompt,
            "n_predict": self.max_tokens,
            "temperature": self.temperature,
            "stream": false,
        })
        .to_string();

        let response = self.http.post_json("/completion", &body)?;
        parse_completion(&response.body)
    }

    /// Which model is loaded, if any. `None` means the server is up but has
    /// nothing resident -- a real state, since models load lazily.
    pub fn loaded_model(&self) -> Result<Option<String>> {
        let response = self.http.get("/props")?;
        let parsed: serde_json::Value = serde_json::from_str(&response.body)
            .map_err(|e| ModelError::Protocol(format!("/props is not JSON: {e}")))?;
        Ok(parsed
            .get("model_path")
            .or_else(|| {
                parsed
                    .get("default_generation_settings")
                    .and_then(|s| s.get("model"))
            })
            .and_then(|v| v.as_str())
            .map(|s| {
                // The full path is noise in a status line; the file name is the
                // part a human recognises.
                s.rsplit('/').next().unwrap_or(s).to_string()
            }))
    }
}

/// llama.cpp's shape, parsed defensively.
///
/// Field names have moved between releases and telemetry is optional, so every
/// field here is optional and missing ones become zero. Losing a statistic must
/// never turn a working completion into an error.
#[derive(Debug, Deserialize)]
struct RawCompletion {
    #[serde(default)]
    content: String,
    #[serde(default)]
    tokens_evaluated: u32,
    #[serde(default)]
    tokens_cached: u32,
    #[serde(default)]
    tokens_predicted: u32,
    #[serde(default)]
    stopped_limit: bool,
    #[serde(default)]
    timings: Option<RawTimings>,
}

#[derive(Debug, Deserialize)]
struct RawTimings {
    #[serde(default)]
    prompt_ms: f64,
    #[serde(default)]
    predicted_ms: f64,
    #[serde(default)]
    prompt_n: u32,
    #[serde(default)]
    predicted_n: u32,
}

fn parse_completion(body: &str) -> Result<Completion> {
    let raw: RawCompletion = serde_json::from_str(body).map_err(|e| {
        let preview: String = body.chars().take(200).collect();
        ModelError::Protocol(format!("{e} (body began: {preview:?})"))
    })?;

    let timings = raw.timings.unwrap_or(RawTimings {
        prompt_ms: 0.0,
        predicted_ms: 0.0,
        prompt_n: 0,
        predicted_n: 0,
    });

    // Prefer the top-level counters; fall back to timings when a build reports
    // only those.
    let evaluated = if raw.tokens_evaluated > 0 {
        raw.tokens_evaluated
    } else {
        timings.prompt_n
    };
    let generated = if raw.tokens_predicted > 0 {
        raw.tokens_predicted
    } else {
        timings.predicted_n
    };

    Ok(Completion {
        text: raw.content,
        stats: CacheStats {
            evaluated,
            cached: raw.tokens_cached,
            generated,
            prefill_ms: timings.prompt_ms as u64,
            generate_ms: timings.predicted_ms as u64,
        },
        truncated: raw.stopped_limit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_response() {
        let body = r#"{
            "content": "snapshot + schedule",
            "tokens_evaluated": 40,
            "tokens_cached": 2960,
            "tokens_predicted": 12,
            "stopped_limit": false,
            "timings": {"prompt_ms": 210.5, "predicted_ms": 900.0,
                        "prompt_n": 40, "predicted_n": 12}
        }"#;
        let completion = parse_completion(body).unwrap();
        assert_eq!(completion.text, "snapshot + schedule");
        assert_eq!(completion.stats.prompt_tokens(), 3000);
        assert_eq!(completion.stats.cached, 2960);
        assert_eq!(completion.stats.prefill_ms, 210);
        assert!(!completion.truncated);
    }

    #[test]
    fn cache_hit_ratio_reflects_reuse() {
        let stats = CacheStats {
            evaluated: 40,
            cached: 2960,
            ..Default::default()
        };
        assert!(
            (stats.hit_ratio() - 0.9867).abs() < 0.001,
            "{}",
            stats.hit_ratio()
        );
        assert!(stats.describe().contains("99%"), "{}", stats.describe());
    }

    #[test]
    fn a_cold_cache_reports_zero_reuse() {
        let stats = CacheStats {
            evaluated: 3000,
            cached: 0,
            ..Default::default()
        };
        assert_eq!(stats.hit_ratio(), 0.0);
        assert_eq!(stats.prompt_tokens(), 3000);
    }

    #[test]
    fn empty_stats_do_not_divide_by_zero() {
        assert_eq!(CacheStats::default().hit_ratio(), 0.0);
    }

    #[test]
    fn missing_telemetry_still_yields_the_text() {
        // Older builds omit timings entirely. Losing a statistic must not turn
        // a working completion into an error.
        let completion = parse_completion(r#"{"content":"hello"}"#).unwrap();
        assert_eq!(completion.text, "hello");
        assert_eq!(completion.stats, CacheStats::default());
    }

    #[test]
    fn falls_back_to_timings_when_top_level_counters_are_absent() {
        let body = r#"{"content":"x","timings":{"prompt_n":17,"predicted_n":3}}"#;
        let stats = parse_completion(body).unwrap().stats;
        assert_eq!(stats.evaluated, 17);
        assert_eq!(stats.generated, 3);
    }

    #[test]
    fn truncation_is_surfaced() {
        // A plan cut off at the token limit is not a plan; callers that parse
        // structured output must be able to tell.
        let body = r#"{"content":"{\"parts\":[","stopped_limit":true}"#;
        assert!(parse_completion(body).unwrap().truncated);
    }

    #[test]
    fn garbage_is_a_protocol_error_that_shows_the_body() {
        let err = parse_completion("<html>502 Bad Gateway</html>").unwrap_err();
        let text = err.to_string();
        assert!(text.contains("502"), "shows what came back: {text}");
    }
}
