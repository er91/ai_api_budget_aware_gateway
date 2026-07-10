use crate::gateway_error::GatewayError;

/// Turns a chunked byte stream into complete lines, buffering any partial trailing line across
/// `feed` calls until a newline arrives. `pos` marks how much of `buf` has already been consumed
/// by `next_line`; that prefix is only actually dropped from `buf` lazily, on the next `feed`,
/// to avoid re-shifting the buffer on every line.
pub struct SseReader {
    buf: Vec<u8>,
    pos: usize,
}

impl SseReader {
    pub fn new() -> Self {
        SseReader {
            buf: Vec::new(),
            pos: 0,
        }
    }

    pub fn feed(&mut self, chunk: &[u8]) {
        if self.pos > 0 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        self.buf.extend_from_slice(chunk);
    }

    pub fn next_line(&mut self) -> Option<Result<String, GatewayError>> {
        let rel_nl = self.buf[self.pos..].iter().position(|&b| b == b'\n')?;
        let start = self.pos;
        let end = start + rel_nl;
        let line = &self.buf[start..end];
        let line_str = match std::str::from_utf8(line) {
            Ok(s) => s,
            Err(e) => {
                return Some(Err(GatewayError::new(
                    500,
                    format!("invalid utf-8 in sse line: {}", e),
                )))
            }
        };
        let line_str = line_str.strip_suffix('\r').unwrap_or(line_str);
        let owned = line_str.to_string();
        self.pos = end + 1;
        Some(Ok(owned))
    }

    pub fn trailing_str(&self) -> Result<String, GatewayError> {
        let s = std::str::from_utf8(&self.buf[self.pos..]).map_err(|e| {
            GatewayError::new(500, format!("invalid utf-8 in trailing data: {}", e))
        })?;
        Ok(s.to_string())
    }
}
