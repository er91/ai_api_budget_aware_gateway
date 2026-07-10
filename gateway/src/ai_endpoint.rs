use std::future::Future;

use worker::{ByteStream, Context, Request, Response, RouteContext};

use crate::gateway_error::GatewayError;

/// Provider-specific hooks used by the generalized proxying flow in `proxy::handle_request`.
pub trait AiEndpoint {
    /// Data computed while building the upstream request that `calculate_cost` needs later, e.g. the parsed model (and thus its pricing table).
    type PricingContext: 'static;

    /// Extracts the gateway token identifying the caller from the incoming request (e.g. an `Authorization` or `X-Api-Key` header).
    fn get_token(req: &Request) -> impl Future<Output = Result<String, GatewayError>>;

    /// Turns the client's request into the upstream request that will be sent to `upstream_url`, and builds the `PricingContext`.
    fn build_upstream_request(
        req: &mut Request,
        upstream_url: &str,
        ctx: &RouteContext<Context>,
    ) -> impl Future<Output = Result<(Self::PricingContext, Request), GatewayError>>;

    /// Reads the upstream response body (`stream`, either a single JSON object or an SSE event stream depending on `is_sse`) to extract token usage, and converts it to a dollar cost using the pricing in `pricing_ctx`.
    /// An `Err` here does not fail the request (the response was already returned to the client by the time this runs) — it's only logged, and the request's usage goes unbilled. This happens, for example, when the model isn't in the provider's pricing table.
    fn calculate_cost(
        pricing_ctx: Self::PricingContext,
        stream: ByteStream,
        is_sse: bool,
    ) -> impl Future<Output = Result<f64, GatewayError>>;

    /// Renders a `GatewayError` into a provider-shaped error `Response`.
    fn generate_error_response(error: &GatewayError) -> Response;

    /// Short identifier for this provider (e.g. `"anthropic"`), used as the D1 lookup key for per-provider token authorization and cost accounting.
    fn provider() -> &'static str;
}
