use crate::ai_endpoint::AiEndpoint;
use crate::gateway_error::GatewayError;
use crate::token;
use worker::{
    console_error, console_log, Context, D1Database, Fetch, Request, Response, RouteContext,
};

/// Generalized gateway proxying flow shared by all `AiEndpoint` providers. Steps:
/// 1. Extract the caller's token via `E::get_token`.
/// 2. Authorize the token against D1 (see `authorize`).
/// 3. Build the upstream request via `E::build_upstream_request` and send it.
/// 4. Pass through non-2xx upstream responses as-is (error body is logged, not parsed for cost).
/// 5. Clone the success response, return the original to the client immediately, and compute
///    cost from the clone + update the token's balance in D1 in the background via
///    `ctx.data.wait_until` so billing never adds latency to the client response.
pub async fn handle_request<E: AiEndpoint>(
    mut req: Request,
    ctx: &RouteContext<Context>,
    upstream_url: &str,
) -> Response {
    let provider = E::provider();
    let user_token = match E::get_token(&req).await {
        Ok(t) => t,
        Err(e) => return E::generate_error_response(&e),
    };

    let db = match ctx.env.d1("DB") {
        Ok(db) => db,
        Err(e) => {
            return E::generate_error_response(&GatewayError::new(
                500,
                format!("get D1 binding failed, original error: {e:?}"),
            ));
        }
    };

    // CF-Connecting-IP is set by Cloudflare's edge network from the real TCP connection, not
    // copied from a client-supplied header — so unlike X-Forwarded-For, the client cannot spoof
    // it to bypass the IP allowlist.
    let client_ip = match req.headers().get("CF-Connecting-IP") {
        Ok(Some(ip)) => ip,
        Ok(None) => {
            return E::generate_error_response(&GatewayError::new(
                401,
                "missing CF-Connecting-IP header",
            ));
        }
        Err(e) => {
            return E::generate_error_response(&GatewayError::new(
                500,
                format!("read CF-Connecting-IP header failed: {:?}", e),
            ));
        }
    };

    // This is a preflight check, not a reservation: the eventual usage is unknown until the
    // upstream response arrives, and concurrent requests can therefore spend past the budget
    // before their background updates are applied.
    if let Err(e) = authorize(&db, provider, &user_token, &client_ip).await {
        return E::generate_error_response(&e);
    }

    let (pricing_ctx, upstream) = match E::build_upstream_request(&mut req, upstream_url, ctx).await
    {
        Ok(r) => r,
        Err(e) => return E::generate_error_response(&e),
    };

    let mut resp = match Fetch::Request(upstream).send().await {
        Ok(r) => r,
        Err(e) => {
            return E::generate_error_response(&GatewayError::new(
                502,
                format!("upstream request failed, original error: {e:?}"),
            ));
        }
    };

    if resp.status_code() >= 400 {
        if let Ok(mut clone) = resp.cloned() {
            ctx.data.wait_until(async move {
                match clone.text().await {
                    Ok(body) => console_error!("upstream error body: {}", body),
                    Err(e) => console_error!("failed to read upstream error body: {:?}", e),
                }
            });
        }
        return resp;
    }

    let content_type = resp
        .headers()
        .get("Content-Type")
        .unwrap_or(None)
        .unwrap_or_default();
    let is_sse = match content_type {
        ct if ct.contains("text/event-stream") => true,
        ct if ct.contains("application/json") => false,
        _ => {
            console_error!("unexpected upstream Content-Type: {}", content_type);
            return resp;
        }
    };

    let mut resp_clone = match resp.cloned() {
        Ok(c) => c,
        Err(e) => {
            console_error!("failed to clone response for cost tracking: {:?}", e);
            return resp;
        }
    };
    let provider = provider.to_string();
    // Billing is deliberately decoupled from the client response. `E::calculate_cost` runs
    // asynchronously inside this wait_until task, after `resp` has already been returned to the
    // caller above — so if it fails (e.g. the usage couldn't be parsed), the request itself does
    // NOT fail; the error is only logged below, and that request's usage simply goes unbilled.
    ctx.data.wait_until(async move {
        let result: Result<(), GatewayError> = async {
            let resp_stream = resp_clone.stream().map_err(|e| {
                GatewayError::new(500, format!("read response stream failed: {:?}", e))
            })?;
            let cost = E::calculate_cost(pricing_ctx, resp_stream, is_sse).await?;
            if cost > 0.0 {
                token::update_cost(&db, &user_token, cost, &provider)
                    .await
                    .map_err(|e| {
                        GatewayError::new(500, format!("update cost failed, original error: {e:?}"))
                    })?;
                console_log!("cost={}", cost);
            } else {
                console_log!("cost={} (skipped db update)", cost);
            }
            Ok(())
        }
        .await;
        if let Err(e) = result {
            console_error!("cost update failed: {:?} (code={})", e.msg, e.code);
        }
    });

    resp
}

/// Rejects the request unless `user_token` is a valid, unexpired, under-budget token for
/// `provider` (see `token::get_token`) and `client_ip` is on that token's IP allowlist — an
/// allowlist containing `"*"` permits any IP.
async fn authorize(
    db: &D1Database,
    provider: &str,
    user_token: &str,
    client_ip: &str,
) -> Result<(), GatewayError> {
    let row = match token::get_token(db, user_token, provider).await {
        Ok(r) => r,
        Err(e) => {
            return Err(GatewayError::new(
                500,
                format!("get token failed, original error: {e:?}"),
            ))
        }
    };
    let row = match row {
        Some(r) => r,
        None => return Err(GatewayError::new(401, "invalid or expired token")),
    };

    let allowed_ips: Vec<String> = match serde_json::from_str(&row.allowed_ips) {
        Ok(ips) => ips,
        Err(e) => {
            return Err(GatewayError::new(
                500,
                format!("parse allowed_ips failed, original error: {e:?}"),
            ))
        }
    };
    if !allowed_ips.iter().any(|ip| ip == "*") && !allowed_ips.iter().any(|ip| ip == client_ip) {
        return Err(GatewayError::new(401, "ip not allowed"));
    }

    Ok(())
}
