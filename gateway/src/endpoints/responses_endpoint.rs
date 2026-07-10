use std::marker::PhantomData;

use futures_util::StreamExt;
use serde_json::Value;
use worker::{
    console_log, ByteStream, Context, Method, Request, RequestInit, Response, RouteContext,
};

use crate::ai_endpoint::AiEndpoint;
use crate::endpoints::sse_reader::SseReader;
use crate::gateway_error::GatewayError;

pub trait PricingContext: Sized {
    /// Builds the pricing context from the parsed request body.
    fn parse(body: &Value) -> Result<Self, String>;

    /// Computes the dollar cost of a response from its `usage` JSON field.
    fn calculate_cost(&self, usage: &Value) -> Result<f64, String>;
}

pub trait EndpointConfig {
    fn api_key_secret() -> &'static str;
    fn provider() -> &'static str;
}

/// `AiEndpoint` implementation for any provider compatible with OpenAI's `/responses` API.
/// `P` is the provider's pricing context: implement `PricingContext` to parse it from the request
/// body and price a response's usage.
/// `C` is the provider's static config: implement `EndpointConfig` to name its API key secret and
/// provider name.
pub struct Endpoint<P, C>(PhantomData<(P, C)>);

impl<P: PricingContext + 'static, C: EndpointConfig> AiEndpoint for Endpoint<P, C> {
    type PricingContext = P;

    /// Extracts the gateway token from the `Authorization: Bearer <token>` header.
    async fn get_token(req: &Request) -> Result<String, GatewayError> {
        let header = req
            .headers()
            .get("Authorization")
            .map_err(|e| {
                GatewayError::new(
                    500,
                    format!("read Authorization header failed, original error: {e:?}"),
                )
            })?
            .ok_or_else(|| GatewayError::new(401, "missing Authorization header"))?;
        let token = header
            .strip_prefix("Bearer ")
            .ok_or_else(|| GatewayError::new(401, "invalid Authorization header"))?;
        Ok(token.to_string())
    }

    /// Validates the request's `model` field and swaps the gateway token in the `Authorization`
    /// header for the real upstream API key (fetched via `ctx.secret(C::api_key_secret())`)
    /// before forwarding.
    async fn build_upstream_request(
        req: &mut Request,
        upstream_url: &str,
        ctx: &RouteContext<Context>,
    ) -> Result<(Self::PricingContext, Request), GatewayError> {
        let body = req.text().await.map_err(|e| {
            GatewayError::new(
                500,
                format!("read request body failed, original error: {e:?}"),
            )
        })?;
        let headers = req.headers().clone();

        let v: Value = serde_json::from_str(&body).map_err(|e| {
            GatewayError::new(
                400,
                format!("parse request body failed, original error: {e:?}"),
            )
        })?;
        let pricing_ctx = P::parse(&v).map_err(|e| GatewayError::new(400, e))?;

        let upstream = build_upstream::<C>(headers, &body, ctx, upstream_url)?;

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
        error.to_openai_error_response()
    }

    fn provider() -> &'static str {
        C::provider()
    }
}

struct UsageParser {
    reader: SseReader,
    is_sse: bool,
    last_usage: Option<Value>,
}

/// For non-streaming responses, `finish` parses the full JSON body and reads the top-level
/// `usage` field. For streaming (SSE) responses, `feed` parses each event and captures the
/// `response.usage` from the `response.completed` event.
impl UsageParser {
    fn new(is_sse: bool) -> Self {
        UsageParser {
            reader: SseReader::new(),
            is_sse,
            last_usage: None,
        }
    }

    fn feed(&mut self, chunk: &[u8]) -> Result<(), GatewayError> {
        self.reader.feed(chunk);
        while let Some(line) = self.reader.next_line() {
            let line_str = line?;
            // A blank line terminates each SSE record and is expected between
            // every event, not an error.
            if line_str.is_empty() {
                continue;
            }
            // The Responses API prefixes each event with an `event: <type>` line before
            // its `data: ` line; the type is also embedded in the data payload, so this
            // line carries no information we need.
            if line_str.starts_with("event: ") {
                continue;
            }
            if let Some(data) = line_str.strip_prefix("data: ") {
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }
                let v = serde_json::from_str::<Value>(data)
                    .map_err(|e| format!("failed to parse sse data: {}", e))?;
                if v["type"].as_str() == Some("response.completed") {
                    self.last_usage = Some(v["response"]["usage"].clone());
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

    fn finish(mut self) -> Result<Value, GatewayError> {
        if self.is_sse {
            let remaining = self.reader.trailing_str()?;
            if !remaining.is_empty() {
                if let Some(data) = remaining.strip_prefix("data: ") {
                    if !data.is_empty() && data != "[DONE]" {
                        if let Ok(v) = serde_json::from_str::<Value>(data) {
                            if v["type"].as_str() == Some("response.completed") {
                                self.last_usage = Some(v["response"]["usage"].clone());
                            }
                        }
                    }
                }
            }

            match self.last_usage {
                Some(usage) => {
                    console_log!("usage: {}", usage);
                    Ok(usage)
                }
                None => Err(GatewayError::new(
                    500,
                    "no response.completed event with usage found",
                )),
            }
        } else {
            let body = self.reader.trailing_str()?;
            let v: Value = serde_json::from_str(&body)
                .map_err(|e| format!("failed to parse response body: {}", e))?;
            let usage = v["usage"].clone();
            console_log!("usage: {}", usage);
            Ok(usage)
        }
    }
}

fn build_upstream<C: EndpointConfig>(
    headers: worker::Headers,
    body: &str,
    ctx: &RouteContext<Context>,
    upstream_url: &str,
) -> Result<Request, GatewayError> {
    let api_key = ctx
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

    headers
        .set("Authorization", &format!("Bearer {}", api_key))
        .map_err(|e| {
            GatewayError::new(
                500,
                format!("set Authorization header failed, original error: {e:?}"),
            )
        })?;

    let init = RequestInit {
        body: Some(body.into()),
        headers,
        method: Method::Post,
        ..Default::default()
    };

    Request::new_with_init(upstream_url, &init).map_err(|e| {
        GatewayError::new(
            500,
            format!("build upstream request failed, original error: {e:?}"),
        )
    })
}
