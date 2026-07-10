use std::marker::PhantomData;

use futures_util::StreamExt;
use serde_json::Value;
use worker::{
    console_log, ByteStream, Context, Method, Request, RequestInit, Response, RouteContext,
};

use crate::ai_endpoint::AiEndpoint;
use crate::endpoints::sse_reader::SseReader;
use crate::gateway_error::GatewayError;

pub trait EndpointConfig {
    fn api_key_secret() -> &'static str;
    fn oauth_token_secret() -> &'static str;
    fn provider() -> &'static str;
}

pub trait PricingContext: Sized {
    /// Builds the pricing context from the parsed request body.
    fn parse(body: &Value) -> Result<Self, String>;

    /// Computes the dollar cost of a response from its parsed `Usage`.
    fn calculate_cost(&self, usage: &Usage) -> Result<f64, String>;
}

/// `AiEndpoint` implementation for any provider compatible with Anthropic's `/v1/messages` API.
/// `P` is the provider's pricing context: implement `PricingContext` to parse it from the request
/// body and price a response's usage.
/// `C` is the provider's static config: implement `EndpointConfig` to name its API key secret, OAuth
/// token secret, and provider name.
pub struct Endpoint<P, C>(PhantomData<(P, C)>);

impl<P: PricingContext + 'static, C: EndpointConfig> AiEndpoint for Endpoint<P, C> {
    type PricingContext = P;

    /// Extracts the gateway token from either the `Authorization: Bearer <token>` header or the
    /// `X-Api-Key` header.
    async fn get_token(req: &Request) -> Result<String, GatewayError> {
        let (token, _) = extract_token(req)?;
        Ok(token)
    }

    /// Validates the request's `model` field and swaps the gateway token in the `Authorization`
    /// or `X-Api-Key` header for the real upstream API key or OAuth token (fetched via
    /// `ctx.secret(..)`, depending on which header the caller used) before forwarding.
    async fn build_upstream_request(
        req: &mut Request,
        upstream_url: &str,
        ctx: &RouteContext<Context>,
    ) -> Result<(Self::PricingContext, Request), GatewayError> {
        let body = req
            .text()
            .await
            .map_err(|e| GatewayError::new(500, format!("read request body failed: {:?}", e)))?;
        let headers = req.headers().clone();

        let (_, token_type) = extract_token(req)?;

        let v: Value = serde_json::from_str(&body).map_err(|e| {
            GatewayError::new(
                400,
                format!("parse request body failed, original error: {e:?}"),
            )
        })?;
        let pricing_ctx = P::parse(&v).map_err(|e| GatewayError::new(400, e))?;

        match token_type {
            TokenType::XApiKeyHeader => {
                let key = ctx
                    .secret(C::api_key_secret())
                    .map_err(|e| {
                        GatewayError::new(
                            500,
                            format!(
                                "get secret {} failed, original error: {e:?}",
                                C::api_key_secret()
                            ),
                        )
                    })?
                    .to_string();
                headers.set("X-Api-Key", &key).map_err(|e| {
                    GatewayError::new(
                        500,
                        format!("set X-Api-Key header failed, original error: {e:?}"),
                    )
                })?;
            }
            TokenType::AuthorizationHeader => {
                let oauth_token = ctx
                    .secret(C::oauth_token_secret())
                    .map_err(|e| {
                        GatewayError::new(
                            500,
                            format!(
                                "get secret {} failed, original error: {e:?}",
                                C::oauth_token_secret()
                            ),
                        )
                    })?
                    .to_string();
                headers
                    .set("Authorization", &format!("Bearer {}", oauth_token))
                    .map_err(|e| {
                        GatewayError::new(
                            500,
                            format!("set Authorization header failed, original error: {e:?}"),
                        )
                    })?;
            }
        }

        let init = RequestInit {
            body: Some(body.into()),
            headers,
            method: Method::Post,
            ..Default::default()
        };

        let upstream = Request::new_with_init(upstream_url, &init).map_err(|e| {
            GatewayError::new(
                500,
                format!("build upstream request failed, original error: {e:?}"),
            )
        })?;

        Ok((pricing_ctx, upstream))
    }

    async fn calculate_cost(
        pricing_ctx: Self::PricingContext,
        mut resp_stream: ByteStream,
        is_sse: bool,
    ) -> Result<f64, GatewayError> {
        let mut parser = UsageParser::new(is_sse);
        while let Some(chunk) = resp_stream.next().await {
            parser
                .feed(&chunk.map_err(|e| {
                    GatewayError::new(500, format!("stream read error: {:?}", e))
                })?)?;
        }
        let usage = parser.finish()?;
        pricing_ctx
            .calculate_cost(&usage)
            .map_err(|e| GatewayError::new(500, e))
    }

    fn generate_error_response(error: &GatewayError) -> Response {
        let body = serde_json::json!({
            "type": "error",
            "error": { "type": anthropic_error_type(error.code), "message": &error.msg },
        });
        Response::from_json(&body)
            .expect("json error body is always serializable")
            .with_status(error.code)
    }

    fn provider() -> &'static str {
        C::provider()
    }
}

#[derive(Debug, Clone, Copy)]
enum TokenType {
    AuthorizationHeader,
    XApiKeyHeader,
}

#[derive(Debug, serde::Serialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_write_5m_input_tokens: u64,
    pub cache_write_1h_input_tokens: u64,
}

fn extract_token(req: &Request) -> Result<(String, TokenType), GatewayError> {
    let auth = req.headers().get("Authorization").map_err(|e| {
        GatewayError::new(
            500,
            format!("read Authorization header failed, original error: {e:?}"),
        )
    })?;
    let api_key = req.headers().get("X-Api-Key").map_err(|e| {
        GatewayError::new(
            500,
            format!("read X-Api-Key header failed, original error: {e:?}"),
        )
    })?;

    match (auth, api_key) {
        (Some(_), Some(_)) => {
            // Anthropic accepts requests with both headers set and silently
            // picks one; rather than depend on that undocumented behavior,
            // reject the request outright.
            Err(GatewayError::new(
                400,
                "must not set both Authorization and X-Api-Key headers",
            ))
        }
        (Some(auth), None) => {
            let token = auth
                .strip_prefix("Bearer ")
                .ok_or_else(|| GatewayError::new(401, "invalid Authorization header"))?;
            Ok((token.to_string(), TokenType::AuthorizationHeader))
        }
        (None, Some(key)) => Ok((key, TokenType::XApiKeyHeader)),
        (None, None) => Err(GatewayError::new(
            401,
            "missing Authorization or X-Api-Key header",
        )),
    }
}

fn anthropic_error_type(status: u16) -> &'static str {
    match status {
        400 => "invalid_request_error",
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        429 => "rate_limit_error",
        _ => "api_error",
    }
}

struct UsageParser {
    reader: SseReader,
    is_sse: bool,
    saw_usage: bool,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_input_tokens: u64,
    cache_creation_5m: u64,
    cache_creation_1h: u64,
    cache_creation_input_tokens: u64,
}

/// For non-streaming responses, `finish` parses the full JSON body directly and reads its
/// top-level `usage` field. For streaming (SSE) responses, `feed` parses each event as it
/// arrives and merges the `usage` field from `message_start` and `message_delta` events (later
/// fields overwrite earlier ones, and `message_delta`'s usage is cumulative), so `finish` just
/// returns the accumulated totals.
impl UsageParser {
    fn new(is_sse: bool) -> Self {
        UsageParser {
            reader: SseReader::new(),
            is_sse,
            saw_usage: false,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m: 0,
            cache_creation_1h: 0,
            cache_creation_input_tokens: 0,
        }
    }

    fn feed(&mut self, chunk: &[u8]) -> Result<(), GatewayError> {
        self.reader.feed(chunk);
        while let Some(line) = self.reader.next_line() {
            let line_str = line?;
            // A blank line terminates each SSE record and is expected between
            // every event, not an error.
            if line_str.is_empty() || line_str.starts_with("event: ") {
                continue;
            } else if let Some(data) = line_str.strip_prefix("data: ") {
                if !data.is_empty() {
                    let v = serde_json::from_str::<Value>(data)
                        .map_err(|e| format!("failed to parse sse data: {}", e))?;
                    let maybe_u = match v["type"].as_str().unwrap_or("") {
                        "message_start" => Some(&v["message"]["usage"]),
                        "message_delta" => Some(&v["usage"]),
                        _ => None,
                    };
                    if let Some(u) = maybe_u {
                        console_log!(
                            "usage event: {}",
                            serde_json::to_string(u).unwrap_or_default()
                        );
                        self.merge(u);
                    }
                }
            } else {
                return Err(GatewayError::new(
                    502,
                    format!("unexpected sse line: {}", line_str),
                ));
            }
        }
        Ok(())
    }

    fn merge(&mut self, u: &Value) {
        if u.is_null() {
            return;
        }
        self.saw_usage = true;
        if let Some(v) = u["input_tokens"].as_u64() {
            self.input_tokens = v;
        }
        if let Some(v) = u["output_tokens"].as_u64() {
            self.output_tokens = v;
        }
        if let Some(v) = u["cache_read_input_tokens"].as_u64() {
            self.cache_read_input_tokens = v;
        }
        if let Some(v) = u["cache_creation_input_tokens"].as_u64() {
            self.cache_creation_input_tokens = v;
        }
        let cc = &u["cache_creation"];
        if !cc.is_null() {
            if let Some(v) = cc["ephemeral_5m_input_tokens"].as_u64() {
                self.cache_creation_5m = v;
            }
            if let Some(v) = cc["ephemeral_1h_input_tokens"].as_u64() {
                self.cache_creation_1h = v;
            }
        }
    }

    fn finish(mut self) -> Result<Usage, GatewayError> {
        if self.is_sse {
            let remaining = self.reader.trailing_str()?;
            if !remaining.is_empty() {
                if let Some(data) = remaining.strip_prefix("data: ") {
                    if !data.is_empty() {
                        if let Ok(v) = serde_json::from_str::<Value>(data) {
                            match v["type"].as_str().unwrap_or("") {
                                "message_start" => self.merge(&v["message"]["usage"]),
                                "message_delta" => self.merge(&v["usage"]),
                                _ => {}
                            }
                        }
                    }
                }
            }
        } else {
            let body = self.reader.trailing_str()?;
            let v: Value = serde_json::from_str(&body)
                .map_err(|e| format!("failed to parse response body: {}", e))?;
            self.merge(&v["usage"]);
        }

        if !self.saw_usage {
            return Err(GatewayError::new(500, "no usage data found in response"));
        }

        self.build()
    }

    fn build(mut self) -> Result<Usage, GatewayError> {
        // Older responses only report the flat `cache_creation_input_tokens` field, without the
        // newer per-TTL `cache_creation.ephemeral_{5m,1h}_input_tokens` breakdown. When neither
        // TTL bucket was populated, treat that flat count as a 5m write (the API's old default).
        if self.cache_creation_5m == 0 && self.cache_creation_1h == 0 {
            self.cache_creation_5m = self.cache_creation_input_tokens;
        }

        let result = Usage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_read_input_tokens: self.cache_read_input_tokens,
            cache_write_5m_input_tokens: self.cache_creation_5m,
            cache_write_1h_input_tokens: self.cache_creation_1h,
        };
        console_log!(
            "final usage: {}",
            serde_json::to_string(&result).unwrap_or_default()
        );
        Ok(result)
    }
}
