use std::os::fd::AsRawFd;

use tokio::net::TcpStream;

use super::hpack;

const HTTP2_PREFACE: &[u8; 24] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

/// HTTP/2 frame types we care about.
const FRAME_TYPE_HEADERS: u8 = 0x01;

/// HEADERS frame flag bits.
const HEADERS_FLAG_PADDED: u8 = 0x08;
const HEADERS_FLAG_PRIORITY: u8 = 0x20;

pub enum HttpConnectionType {
    UNSPECIFIED,
    HTTP1,
    HTTP2,
}

pub struct HttpConnection {
    pub connection_type: HttpConnectionType,
    pub raw_path: String,
    pub service_name: String,
    pub tcp_stream: TcpStream,
}

struct PartialHttpConnection {
    connection_type: HttpConnectionType,
    raw_path: String,
    service_name: String,
}

impl Default for PartialHttpConnection {
    fn default() -> Self {
        Self { connection_type: HttpConnectionType::UNSPECIFIED, raw_path: Default::default(), service_name: Default::default() }
    }
}

impl HttpConnection {
    /// Peek at an accepted TCP stream to determine protocol and extract the
    /// request path **without consuming any bytes** from the stream.  The
    /// underlying file descriptor can therefore be handed off to another
    /// process unmodified.
    pub async fn from_tcp_stream(stream: TcpStream) -> Result<Self, Box<dyn std::error::Error>> {
        stream.readable().await?;

        let mut peek_buf = [0u8; 4096];
        let partial_http_connection: PartialHttpConnection;

        'reading_loop: loop {
            let n = stream.peek(&mut peek_buf).await?;
            if n == 0 {
                return Err("connection closed".into());
            }

            let buf = &peek_buf[..n];

            // We need at least a handful of bytes before we can decide anything.
            if n < 9 {
                // Not enough data yet.  See comment below about timeout.
                // If fewer than 9 bytes are ever sent this loops forever —
                // the caller should wrap `from_tcp_stream` in a
                // `tokio::time::timeout`.
                continue 'reading_loop;
            }

            // ---------------------------------------------------------------
            // HTTP/1.x detection — check for a known method token.
            // ---------------------------------------------------------------
            if buf.starts_with(b"GET ")
                || buf.starts_with(b"POST ")
                || buf.starts_with(b"PUT ")
                || buf.starts_with(b"HEAD ")
                || buf.starts_with(b"DELETE ")
            {
                partial_http_connection = Self::parse_http1(buf)?;
                break 'reading_loop;
            }

            // ---------------------------------------------------------------
            // HTTP/2 detection — look for the 24-byte connection preface.
            // ---------------------------------------------------------------
            if n >= 24 && buf[..24] == *HTTP2_PREFACE {
                match Self::parse_http2(buf) {
                    Ok(Some(conn)) => {
                        partial_http_connection = conn;
                        break 'reading_loop;
                    },
                    Ok(None) => continue, // need more data from the socket
                    Err(e) => return Err(e),
                }
            }

            // If we have ≥24 bytes and nothing matched, it is not a protocol
            // we understand.
            if n >= 24 {
                return Err("unknown protocol".into());
            }
        }

        match partial_http_connection.connection_type {
            HttpConnectionType::UNSPECIFIED => panic!("partial http connection never set in reading loop"),
            HttpConnectionType::HTTP1 => {},
            HttpConnectionType::HTTP2 => {},
        };

        return Ok(Self {
            connection_type: partial_http_connection.connection_type,
            raw_path: partial_http_connection.raw_path,
            service_name: partial_http_connection.service_name,
            tcp_stream: stream,
        })
    }

    // -----------------------------------------------------------------------
    // HTTP/1.x
    // -----------------------------------------------------------------------

    fn parse_http1(buf: &[u8]) -> Result<PartialHttpConnection, Box<dyn std::error::Error>> {
        // RFC 2616 §5.1  Request-Line = Method SP Request-URI SP HTTP-Version CRLF
        let parts: Vec<&[u8]> = buf.splitn(4, |b| *b == b' ').collect();
        if parts.len() < 3 {
            return Err("invalid HTTP/1 request line".into());
        }

        let target =
            std::str::from_utf8(parts[1]).map_err(|_| "request-target is not valid UTF-8")?;

        if target == "*" {
            return Err("OPTIONS * is not a routable request-target".into());
        }

        let path = target
            .strip_prefix('/')
            .ok_or("expected request-target to start with /")?;

        let service_name = extract_service_name(path);

        Ok(PartialHttpConnection {
            connection_type: HttpConnectionType::HTTP1,
            raw_path: path.to_string(),
            service_name: service_name.to_string(),
        })
    }

    // -----------------------------------------------------------------------
    // HTTP/2
    // -----------------------------------------------------------------------

    /// Try to locate the first HEADERS frame after the connection preface and
    /// extract `:path` from its HPACK-encoded header block.
    ///
    /// Returns `Ok(None)` when the peek buffer doesn't contain the complete
    /// HEADERS frame yet (caller should peek again with more data).
    fn parse_http2(
        buf: &[u8],
    ) -> Result<Option<PartialHttpConnection>, Box<dyn std::error::Error>> {
        // Skip the 24-byte client connection preface.
        let mut pos: usize = 24;

        // Walk frames until we hit a HEADERS frame.
        loop {
            // Need at least 9 bytes for the frame header.
            if pos + 9 > buf.len() {
                return Ok(None);
            }

            let frame_len = u32::from_be_bytes([0, buf[pos], buf[pos + 1], buf[pos + 2]]) as usize;
            let frame_type = buf[pos + 3];
            let flags = buf[pos + 4];
            let frame_end = pos + 9 + frame_len;

            if frame_type != FRAME_TYPE_HEADERS {
                // Not the frame we want — skip it.
                if frame_end > buf.len() {
                    // Frame body not fully available yet.
                    return Ok(None);
                }
                pos = frame_end;
                continue;
            }

            // Found a HEADERS frame — make sure the full payload is buffered.
            if frame_end > buf.len() {
                return Ok(None);
            }

            // Locate the header-block fragment within the HEADERS payload.
            let mut hdr_start = pos + 9;
            let mut hdr_end = frame_end;

            if flags & HEADERS_FLAG_PADDED != 0 {
                if hdr_start >= hdr_end {
                    return Err("HEADERS frame too short for pad length".into());
                }
                let pad_len = buf[hdr_start] as usize;
                hdr_start += 1;
                hdr_end -= pad_len;
            }

            if flags & HEADERS_FLAG_PRIORITY != 0 {
                // 4-byte stream dependency + 1-byte weight.
                hdr_start += 5;
            }

            if hdr_start > hdr_end {
                return Err("HEADERS frame payload too small".into());
            }

            let header_block = &buf[hdr_start..hdr_end];

            let path = hpack::find_path_header(header_block)
                .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

            let trimmed = path.strip_prefix('/').unwrap_or(&path);
            let service_name = extract_service_name(trimmed);

            return Ok(Some(PartialHttpConnection {
                connection_type: HttpConnectionType::HTTP2,
                raw_path: trimmed.to_string(),
                service_name: service_name.to_string(),
            }));
        }
    }
}

/// Given a path with the leading `/` already stripped (e.g.
/// `package.ServiceName/MethodName`), return everything before the final
/// `/` — i.e. the gRPC service name.
fn extract_service_name(path: &str) -> &str {
    match path.rsplit_once('/') {
        Some((service, _method)) => service,
        None => path,
    }
}
