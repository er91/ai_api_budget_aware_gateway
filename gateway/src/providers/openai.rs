use std::str::FromStr;

use serde::Deserialize;
use serde_json::Value;

use crate::endpoints::responses_endpoint::{EndpointConfig, PricingContext};

const PROVIDER: &str = "openai";
const API_KEY_SECRET: &str = "OPENAI_API_KEY";

// OpenCode's built-in `openai` provider and Codex both talk to OpenAI's Responses API by
// default (not `/chat/completions`), which is why this provider only implements the
// `/v1/responses` endpoint.
pub struct OpenAIConfig;

impl EndpointConfig for OpenAIConfig {
    fn api_key_secret() -> &'static str {
        API_KEY_SECRET
    }

    fn provider() -> &'static str {
        PROVIDER
    }
}

#[derive(Debug, Clone, Copy)]
pub enum OpenAIModel {
    Gpt5_6Sol,
    Gpt5_6Terra,
    Gpt5_6Luna,
    Gpt5_5,
    Gpt5_5Pro,
    Gpt5_4,
    Gpt5_4Mini,
    Gpt5_4Nano,
    Gpt5_4Pro,
}

impl FromStr for OpenAIModel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Check variants before their broader family prefixes (e.g. parse
        // `gpt-5.5-pro` before `gpt-5.5`).
        if s.starts_with("gpt-5.6-luna") {
            return Ok(OpenAIModel::Gpt5_6Luna);
        }
        if s.starts_with("gpt-5.6-terra") {
            return Ok(OpenAIModel::Gpt5_6Terra);
        }
        if s.starts_with("gpt-5.6-sol") {
            return Ok(OpenAIModel::Gpt5_6Sol);
        }
        if s.starts_with("gpt-5.5-pro") {
            return Ok(OpenAIModel::Gpt5_5Pro);
        }
        if s.starts_with("gpt-5.5") {
            return Ok(OpenAIModel::Gpt5_5);
        }
        if s.starts_with("gpt-5.4-pro") {
            return Ok(OpenAIModel::Gpt5_4Pro);
        }
        if s.starts_with("gpt-5.4-nano") {
            return Ok(OpenAIModel::Gpt5_4Nano);
        }
        if s.starts_with("gpt-5.4-mini") {
            return Ok(OpenAIModel::Gpt5_4Mini);
        }
        if s.starts_with("gpt-5.4") {
            return Ok(OpenAIModel::Gpt5_4);
        }
        Err(format!("unknown model: {}", s))
    }
}

pub struct OpenAIPricingContext {
    model: OpenAIModel,
}

struct Pricing {
    input_cache_hit: f64,
    input_cache_miss: f64,
    cache_write: f64,
    output: f64,
}

impl OpenAIModel {
    fn pricing(&self) -> Pricing {
        match self {
            OpenAIModel::Gpt5_6Sol => Pricing {
                input_cache_hit: 0.50,
                input_cache_miss: 5.00,
                cache_write: 6.25,
                output: 30.00,
            },
            OpenAIModel::Gpt5_6Terra => Pricing {
                input_cache_hit: 0.25,
                input_cache_miss: 2.50,
                cache_write: 3.125,
                output: 15.00,
            },
            OpenAIModel::Gpt5_6Luna => Pricing {
                input_cache_hit: 0.10,
                input_cache_miss: 1.00,
                cache_write: 1.25,
                output: 6.00,
            },
            OpenAIModel::Gpt5_5 => Pricing {
                input_cache_hit: 0.50,
                input_cache_miss: 5.00,
                cache_write: 0.0,
                output: 30.00,
            },
            OpenAIModel::Gpt5_5Pro => Pricing {
                input_cache_hit: 30.00,
                input_cache_miss: 30.00,
                cache_write: 0.0,
                output: 180.00,
            },
            OpenAIModel::Gpt5_4 => Pricing {
                input_cache_hit: 0.25,
                input_cache_miss: 2.50,
                cache_write: 0.0,
                output: 15.00,
            },
            OpenAIModel::Gpt5_4Mini => Pricing {
                input_cache_hit: 0.075,
                input_cache_miss: 0.75,
                cache_write: 0.0,
                output: 4.50,
            },
            OpenAIModel::Gpt5_4Nano => Pricing {
                input_cache_hit: 0.02,
                input_cache_miss: 0.20,
                cache_write: 0.0,
                output: 1.25,
            },
            OpenAIModel::Gpt5_4Pro => Pricing {
                input_cache_hit: 30.00,
                input_cache_miss: 30.00,
                cache_write: 0.0,
                output: 180.00,
            },
        }
    }
}

#[derive(Debug, Deserialize)]
struct ResponsesUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    input_tokens_details: Option<InputTokenDetails>,
}

#[derive(Debug, Deserialize)]
struct InputTokenDetails {
    #[serde(default)]
    cached_tokens: u64,
    #[serde(default)]
    cache_write_tokens: u64,
}

const LONG_CONTEXT_TOKEN_THRESHOLD: u64 = 272_000;

impl PricingContext for OpenAIPricingContext {
    fn parse(body: &Value) -> Result<Self, String> {
        let model = body["model"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "model is required".to_string())?;
        Ok(OpenAIPricingContext {
            model: model.parse()?,
        })
    }

    fn calculate_cost(&self, usage: &Value) -> Result<f64, String> {
        calculate_cost(&self.model, usage)
    }
}

fn calculate_cost(model: &OpenAIModel, usage: &Value) -> Result<f64, String> {
    let usage: ResponsesUsage = serde_json::from_value(usage.clone())
        .map_err(|e| format!("failed to parse usage: {}", e))?;
    let p = model.pricing();
    let is_long = usage.input_tokens > LONG_CONTEXT_TOKEN_THRESHOLD;
    let input_mult: f64 = if is_long { 2.0 } else { 1.0 };
    let output_mult: f64 = if is_long { 1.5 } else { 1.0 };
    let details = usage.input_tokens_details.as_ref();
    let input_cache_hit = details
        .map(|d| d.cached_tokens.min(usage.input_tokens))
        .unwrap_or(0);
    let cache_write = details.map(|d| d.cache_write_tokens).unwrap_or(0);
    let input_cache_miss = usage
        .input_tokens
        .saturating_sub(input_cache_hit)
        .saturating_sub(cache_write);
    Ok(
        input_cache_hit as f64 / 1_000_000.0 * p.input_cache_hit * input_mult
            + input_cache_miss as f64 / 1_000_000.0 * p.input_cache_miss * input_mult
            + cache_write as f64 / 1_000_000.0 * p.cache_write * input_mult
            + usage.output_tokens as f64 / 1_000_000.0 * p.output * output_mult,
    )
}

pub type ResponsesEndpoint =
    crate::endpoints::responses_endpoint::Endpoint<OpenAIPricingContext, OpenAIConfig>;
