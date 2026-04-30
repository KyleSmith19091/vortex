use tokio::net::TcpStream;

#[derive(Debug, PartialEq)]
pub enum RequestKind {
    /// POST with a base64-encoded body — path is /version/service/route.
    Invocation,
    /// GET for service metadata — path starts with /metadata.
    Metadata,
}

pub struct HttpConnection {
    pub kind: RequestKind,
    pub version: String,
    pub service: String,
    pub route: String,
    pub raw_path: String,
    pub tcp_stream: TcpStream,
}

impl HttpConnection {
    /// Peek at an accepted TCP stream to extract the HTTP/1.1 request method
    /// and path **without consuming any bytes** from the stream.
    pub async fn from_tcp_stream(stream: TcpStream) -> Result<Self, Box<dyn std::error::Error>> {
        stream.readable().await?;

        let mut peek_buf = [0u8; 4096];

        loop {
            let n = stream.peek(&mut peek_buf).await?;
            if n == 0 {
                return Err("connection closed".into());
            }

            let buf = &peek_buf[..n];

            // Need enough bytes to see at least "GET / HTTP/1.1\r\n".
            if n < 16 {
                continue;
            }

            if buf.starts_with(b"POST ") {
                return Self::parse_invocation(buf, stream);
            } else if buf.starts_with(b"GET ") {
                return Self::parse_metadata(buf, stream);
            } else {
                return Err("unsupported HTTP method: only GET and POST are accepted".into());
            }
        }
    }

    /// Parse a POST request as an invocation.
    /// Path must be /version/service/route and body must be valid base64.
    fn parse_invocation(
        buf: &[u8],
        stream: TcpStream,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let path = extract_request_path(buf)?;
        let (version, service, route) = parse_invocation_path(path)?;

        let body = extract_body(buf)?;
        validate_base64(body)?;

        Ok(Self {
            kind: RequestKind::Invocation,
            version: version.to_string(),
            service: service.to_string(),
            route: route.to_string(),
            raw_path: path.to_string(),
            tcp_stream: stream,
        })
    }

    /// Parse a GET request as a metadata lookup.
    /// Path must start with /metadata.
    fn parse_metadata(buf: &[u8], stream: TcpStream) -> Result<Self, Box<dyn std::error::Error>> {
        let path = extract_request_path(buf)?;

        if !path.starts_with("metadata/") && path != "metadata" {
            return Err("GET requests must target /metadata".into());
        }

        Ok(Self {
            kind: RequestKind::Metadata,
            version: String::new(),
            service: String::new(),
            route: String::new(),
            raw_path: path.to_string(),
            tcp_stream: stream,
        })
    }
}

/// Extract the request-target from an HTTP/1.1 request line, stripping the
/// leading `/`.
fn extract_request_path(buf: &[u8]) -> Result<&str, Box<dyn std::error::Error>> {
    // RFC 2616 §5.1  Request-Line = Method SP Request-URI SP HTTP-Version CRLF
    let parts: Vec<&[u8]> = buf.splitn(4, |b| *b == b' ').collect();
    if parts.len() < 3 {
        return Err("invalid HTTP/1.1 request line".into());
    }

    let target =
        std::str::from_utf8(parts[1]).map_err(|_| "request-target is not valid UTF-8")?;

    target
        .strip_prefix('/')
        .ok_or_else(|| "expected request-target to start with /".into())
}

/// Parse a POST path of the form `version/service/route` (leading `/` already
/// stripped).
fn parse_invocation_path(path: &str) -> Result<(&str, &str, &str), Box<dyn std::error::Error>> {
    let segments: Vec<&str> = path.splitn(3, '/').collect();
    if segments.len() != 3 || segments.iter().any(|s| s.is_empty()) {
        return Err(
            format!("invalid path '/{path}': expected format /version/service/route").into(),
        );
    }
    Ok((segments[0], segments[1], segments[2]))
}

/// Locate the body in a raw HTTP/1.1 request buffer (everything after the
/// `\r\n\r\n` header terminator).
fn extract_body(buf: &[u8]) -> Result<&[u8], Box<dyn std::error::Error>> {
    let header_end = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("incomplete HTTP headers in peek buffer")?;
    Ok(&buf[header_end + 4..])
}

/// Validate that `data` is valid standard base64 (RFC 4648 §4).
/// Accepts A-Z, a-z, 0-9, +, /, and trailing `=` padding. Rejects empty body.
fn validate_base64(data: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    if data.is_empty() {
        return Err("POST body must not be empty".into());
    }

    let mut padding_started = false;
    for &b in data {
        if b == b'=' {
            padding_started = true;
        } else if padding_started {
            return Err("invalid base64: data after padding".into());
        } else if !(b.is_ascii_alphanumeric() || b == b'+' || b == b'/') {
            return Err(format!("invalid base64 byte: 0x{b:02x}").into());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- parse_invocation_path ------------------------------------------------

    #[test]
    fn parse_valid_invocation_path() {
        let (v, s, r) = parse_invocation_path("v1/users/list").unwrap();
        assert_eq!(v, "v1");
        assert_eq!(s, "users");
        assert_eq!(r, "list");
    }

    #[test]
    fn parse_invocation_path_with_nested_route() {
        let (v, s, r) = parse_invocation_path("v2/orders/get/123").unwrap();
        assert_eq!(v, "v2");
        assert_eq!(s, "orders");
        assert_eq!(r, "get/123");
    }

    #[test]
    fn parse_invocation_path_missing_segment() {
        assert!(parse_invocation_path("v1/users").is_err());
    }

    #[test]
    fn parse_invocation_path_empty_segment() {
        assert!(parse_invocation_path("v1//method").is_err());
    }

    #[test]
    fn parse_invocation_path_single_segment() {
        assert!(parse_invocation_path("v1").is_err());
    }

    // -- validate_base64 ------------------------------------------------------

    #[test]
    fn base64_valid_no_padding() {
        validate_base64(b"SGVsbG8gV29ybGQ").unwrap();
    }

    #[test]
    fn base64_valid_with_padding() {
        validate_base64(b"SGVsbG8=").unwrap();
    }

    #[test]
    fn base64_valid_double_padding() {
        validate_base64(b"SG8=").unwrap();
    }

    #[test]
    fn base64_invalid_character() {
        assert!(validate_base64(b"SGVs!G8=").is_err());
    }

    #[test]
    fn base64_data_after_padding() {
        assert!(validate_base64(b"SG8=abc").is_err());
    }

    #[test]
    fn base64_empty_body() {
        assert!(validate_base64(b"").is_err());
    }

    // -- extract_body ---------------------------------------------------------

    #[test]
    fn extract_body_from_request() {
        let req = b"POST /v1/svc/op HTTP/1.1\r\nHost: x\r\n\r\nSGVsbG8=";
        let body = extract_body(req).unwrap();
        assert_eq!(body, b"SGVsbG8=");
    }

    #[test]
    fn extract_body_missing_headers_end() {
        let req = b"POST /v1/svc/op HTTP/1.1\r\nHost: x";
        assert!(extract_body(req).is_err());
    }

    // -- extract_request_path -------------------------------------------------

    #[test]
    fn extract_path_get() {
        let req = b"GET /metadata HTTP/1.1\r\n";
        assert_eq!(extract_request_path(req).unwrap(), "metadata");
    }

    #[test]
    fn extract_path_post() {
        let req = b"POST /v1/svc/invoke HTTP/1.1\r\n";
        assert_eq!(extract_request_path(req).unwrap(), "v1/svc/invoke");
    }
}
