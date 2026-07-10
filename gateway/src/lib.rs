use worker::event;
use worker::{Context, Env, Request, Response, Router};

use crate::gateway_error::GatewayError;

mod ai_endpoint;
mod providers {
    pub mod anthropic;
    pub mod deepseek;
    pub mod openai;
}
mod endpoints {
    pub mod chat_completions_endpoint;
    pub mod messages_endpoint;
    pub mod responses_endpoint;
    pub mod sse_reader;
}
mod gateway_error;
mod proxy;
mod token;

const DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com";
const DEEPSEEK_ANTHROPIC_BASE_URL: &str = "https://api.deepseek.com/anthropic";
const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";
const OPENAI_BASE_URL: &str = "https://api.openai.com";

const PATH_PARAM: &str = "path";

#[event(fetch)]
async fn main(req: Request, env: Env, ctx: Context) -> worker::Result<Response> {
    console_error_panic_hook::set_once();

    Router::with_data(ctx)
        .get_async("/", |_req, _ctx| async move {
            Response::ok("API Gateway is running")
        })
        .post_async(&format!("/*{}", PATH_PARAM), |req, ctx| async move {
            let path = ctx.param(PATH_PARAM).unwrap();
            match path.as_str() {
                "deepseek/chat/completions" => {
                    let upstream = format!("{}/chat/completions", DEEPSEEK_BASE_URL);
                    Ok(
                        proxy::handle_request::<providers::deepseek::ChatCompletionsEndpoint>(
                            req, &ctx, &upstream,
                        )
                        .await,
                    )
                }
                "deepseek/v1/messages" => {
                    let upstream = format!("{}/v1/messages", DEEPSEEK_ANTHROPIC_BASE_URL);
                    Ok(
                        proxy::handle_request::<providers::deepseek::MessagesEndpoint>(
                            req, &ctx, &upstream,
                        )
                        .await,
                    )
                }
                "anthropic/v1/messages" => {
                    let upstream = format!("{}/v1/messages", ANTHROPIC_BASE_URL);
                    Ok(
                        proxy::handle_request::<providers::anthropic::MessagesEndpoint>(
                            req, &ctx, &upstream,
                        )
                        .await,
                    )
                }
                "openai/v1/responses" => {
                    let upstream = format!("{}/v1/responses", OPENAI_BASE_URL);
                    Ok(
                        proxy::handle_request::<providers::openai::ResponsesEndpoint>(
                            req, &ctx, &upstream,
                        )
                        .await,
                    )
                }
                _ => {
                    let error = GatewayError::new(404, format!("unsupported path: {}", path));
                    Ok(error.to_openai_error_response())
                }
            }
        })
        .run(req, env)
        .await
}
