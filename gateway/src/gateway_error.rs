use worker::Response;

pub struct GatewayError {
    pub code: u16,
    pub msg: String,
}

impl GatewayError {
    pub fn new(code: u16, msg: impl Into<String>) -> Self {
        GatewayError {
            code,
            msg: msg.into(),
        }
    }

    pub fn to_openai_error_response(&self) -> Response {
        let body = serde_json::json!({
            "error": { "message": &self.msg, "type": "invalid_request_error", "param": null, "code": null },
        });
        Response::from_json(&body)
            .expect("json error body is always serializable")
            .with_status(self.code)
    }
}

/// Lets `?` convert a plain `String` error (e.g. from `map_err(|e| format!(..))?`) into a
/// `GatewayError`, always as a 500 — use `GatewayError::new` directly for other status codes.
impl From<String> for GatewayError {
    fn from(msg: String) -> Self {
        GatewayError { code: 500, msg }
    }
}
