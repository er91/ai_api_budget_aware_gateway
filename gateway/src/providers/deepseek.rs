use std::str::FromStr;

use serde::Deserialize;
use serde_json::Value;

use crate::endpoints::chat_completions_endpoint::{Endpoint, EndpointConfig, PricingContext};
use crate::endpoints::messages_endpoint::{
    EndpointConfig as MessagesEndpointConfig, PricingContext as MessagesPricingContext,
    Usage as MessagesUsage,
};

const PROVIDER: &str = "deepseek";
const API_KEY_SECRET: &str = "DEEPSEEK_API_KEY";

pub struct DeepSeekConfig;

struct Pricing {
    input_cache_hit: f64,
    input_cache_miss: f64,
    output: f64,
}

// DeepSeek's /chat/completions usage shape (`prompt_cache_hit_tokens` /
// `prompt_cache_miss_tokens`) differs from OpenAI's own `/chat/completions` usage shape.
#[derive(Debug, Deserialize)]
struct ChatCompletionsUsage {
    #[serde(default)]
    prompt_cache_hit_tokens: u64,
    #[serde(default)]
    prompt_cache_miss_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

impl EndpointConfig for DeepSeekConfig {
    fn api_key_secret() -> &'static str {
        API_KEY_SECRET
    }

    fn provider() -> &'static str {
        PROVIDER
    }
}

impl MessagesEndpointConfig for DeepSeekConfig {
    fn api_key_secret() -> &'static str {
        API_KEY_SECRET
    }

    // DeepSeek has no separate OAuth flow; it deliberately accepts the same
    // API key via either X-Api-Key or Authorization: Bearer.
    fn oauth_token_secret() -> &'static str {
        API_KEY_SECRET
    }

    fn provider() -> &'static str {
        PROVIDER
    }
}

#[derive(Debug, Clone, Copy)]
pub enum DeepSeekModel {
    V4Flash,
    V4Pro,
}

impl FromStr for DeepSeekModel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "deepseek-v4-flash" => Ok(DeepSeekModel::V4Flash),
            "deepseek-v4-pro" => Ok(DeepSeekModel::V4Pro),
            _ => Err(format!("unknown model: {}", s)),
        }
    }
}

pub struct DeepSeekPricingContext {
    model: DeepSeekModel,
}

impl DeepSeekModel {
    fn pricing(&self) -> Pricing {
        match self {
            DeepSeekModel::V4Flash => Pricing {
                input_cache_hit: 0.0028,
                input_cache_miss: 0.14,
                output: 0.28,
            },
            DeepSeekModel::V4Pro => Pricing {
                input_cache_hit: 0.003625,
                input_cache_miss: 0.435,
                output: 0.87,
            },
        }
    }
}

impl PricingContext for DeepSeekPricingContext {
    fn parse(body: &Value) -> Result<Self, String> {
        let model = body["model"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "model is required".to_string())?;
        Ok(DeepSeekPricingContext {
            model: model.parse()?,
        })
    }

    fn calculate_cost(&self, usage: &Value) -> Result<f64, String> {
        let usage: ChatCompletionsUsage = serde_json::from_value(usage.clone())
            .map_err(|e| format!("failed to parse usage: {}", e))?;
        let p = self.model.pricing();
        Ok(
            usage.prompt_cache_hit_tokens as f64 / 1_000_000.0 * p.input_cache_hit
                + usage.prompt_cache_miss_tokens as f64 / 1_000_000.0 * p.input_cache_miss
                + usage.completion_tokens as f64 / 1_000_000.0 * p.output,
        )
    }
}

impl MessagesPricingContext for DeepSeekPricingContext {
    fn parse(body: &Value) -> Result<Self, String> {
        let model = body["model"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "model is required".to_string())?;
        // DeepSeek's Anthropic-compatible endpoint: claude-opus maps to v4-pro,
        // claude-haiku/claude-sonnet map to v4-flash, native deepseek-* names
        // pass through, and any other unrecognized name defaults to v4-flash.
        if model.starts_with("claude-sonnet") || model.starts_with("claude-haiku") {
            return Ok(DeepSeekPricingContext {
                model: DeepSeekModel::V4Flash,
            });
        }
        if model.starts_with("claude-opus") {
            return Ok(DeepSeekPricingContext {
                model: DeepSeekModel::V4Pro,
            });
        }
        Ok(DeepSeekPricingContext {
            model: model.parse().unwrap_or(DeepSeekModel::V4Flash),
        })
    }

    fn calculate_cost(&self, usage: &MessagesUsage) -> Result<f64, String> {
        let p = self.model.pricing();
        Ok(usage.input_tokens as f64 / 1_000_000.0 * p.input_cache_miss
            + usage.cache_write_5m_input_tokens as f64 / 1_000_000.0 * p.input_cache_miss
            + usage.cache_write_1h_input_tokens as f64 / 1_000_000.0 * p.input_cache_miss
            + usage.cache_read_input_tokens as f64 / 1_000_000.0 * p.input_cache_hit
            + usage.output_tokens as f64 / 1_000_000.0 * p.output)
    }
}

pub type ChatCompletionsEndpoint = Endpoint<DeepSeekPricingContext, DeepSeekConfig>;
pub type MessagesEndpoint =
    crate::endpoints::messages_endpoint::Endpoint<DeepSeekPricingContext, DeepSeekConfig>;
