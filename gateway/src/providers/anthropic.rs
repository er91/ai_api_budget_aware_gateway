use std::str::FromStr;

use serde_json::Value;

use crate::endpoints::messages_endpoint::{Endpoint, EndpointConfig, PricingContext, Usage};

const PROVIDER: &str = "anthropic";
const API_KEY_SECRET: &str = "ANTHROPIC_API_KEY";
const OAUTH_TOKEN_SECRET: &str = "ANTHROPIC_OAUTH_TOKEN";

pub struct AnthropicConfig;

impl EndpointConfig for AnthropicConfig {
    fn api_key_secret() -> &'static str {
        API_KEY_SECRET
    }

    fn oauth_token_secret() -> &'static str {
        OAUTH_TOKEN_SECRET
    }

    fn provider() -> &'static str {
        PROVIDER
    }
}

#[derive(Debug, Clone, Copy)]
pub enum AnthropicModel {
    Haiku4_5,
    Sonnet4_6,
    Sonnet5,
    Opus4_6,
    Opus4_7,
    Opus4_8,
    Fable5,
}

impl FromStr for AnthropicModel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "claude-haiku-4-5-20251001" => Ok(AnthropicModel::Haiku4_5),
            "claude-sonnet-4-6" => Ok(AnthropicModel::Sonnet4_6),
            "claude-sonnet-5" => Ok(AnthropicModel::Sonnet5),
            "claude-opus-4-6" => Ok(AnthropicModel::Opus4_6),
            "claude-opus-4-7" => Ok(AnthropicModel::Opus4_7),
            "claude-opus-4-8" => Ok(AnthropicModel::Opus4_8),
            "claude-fable-5" => Ok(AnthropicModel::Fable5),
            _ => Err(format!("unknown model: {}", s)),
        }
    }
}

pub struct AnthropicPricingContext {
    model: AnthropicModel,
}

struct Pricing {
    input: f64,
    cache_write_5m: f64,
    cache_write_1h: f64,
    cache_hit: f64,
    output: f64,
}

impl AnthropicModel {
    fn pricing(&self) -> Pricing {
        match self {
            AnthropicModel::Haiku4_5 => Pricing {
                input: 1.0,
                cache_write_5m: 1.25,
                cache_write_1h: 2.0,
                cache_hit: 0.10,
                output: 5.0,
            },
            AnthropicModel::Sonnet4_6 => Pricing {
                input: 3.0,
                cache_write_5m: 3.75,
                cache_write_1h: 6.0,
                cache_hit: 0.30,
                output: 15.0,
            },
            // TODO: this is Sonnet 5's introductory pricing, valid through 2026-08-31.
            // After that, standard pricing is $3/$15 per MTok (input/output) — update
            // to input: 3.0, cache_write_5m: 3.75, cache_write_1h: 6.0, cache_hit: 0.30,
            // output: 15.0 (same ratios as Sonnet 4.6).
            AnthropicModel::Sonnet5 => Pricing {
                input: 2.0,
                cache_write_5m: 2.50,
                cache_write_1h: 4.0,
                cache_hit: 0.20,
                output: 10.0,
            },
            AnthropicModel::Opus4_6 | AnthropicModel::Opus4_7 | AnthropicModel::Opus4_8 => {
                Pricing {
                    input: 5.0,
                    cache_write_5m: 6.25,
                    cache_write_1h: 10.0,
                    cache_hit: 0.50,
                    output: 25.0,
                }
            }
            AnthropicModel::Fable5 => Pricing {
                input: 10.0,
                cache_write_5m: 12.50,
                cache_write_1h: 20.0,
                cache_hit: 1.0,
                output: 50.0,
            },
        }
    }
}

impl PricingContext for AnthropicPricingContext {
    fn parse(body: &Value) -> Result<Self, String> {
        let model = body["model"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "model is required".to_string())?;
        Ok(AnthropicPricingContext {
            model: model.parse()?,
        })
    }

    fn calculate_cost(&self, usage: &Usage) -> Result<f64, String> {
        let p = self.model.pricing();
        Ok(usage.input_tokens as f64 / 1_000_000.0 * p.input
            + usage.cache_write_5m_input_tokens as f64 / 1_000_000.0 * p.cache_write_5m
            + usage.cache_write_1h_input_tokens as f64 / 1_000_000.0 * p.cache_write_1h
            + usage.cache_read_input_tokens as f64 / 1_000_000.0 * p.cache_hit
            + usage.output_tokens as f64 / 1_000_000.0 * p.output)
    }
}

pub type MessagesEndpoint = Endpoint<AnthropicPricingContext, AnthropicConfig>;
