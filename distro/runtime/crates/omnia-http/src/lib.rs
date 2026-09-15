//! Minimal HTTP/1.1 client and SSE reader for loopback traffic.
//!
//! Scope is deliberately tiny: POST a JSON body to `127.0.0.1`, read a response
//! or stream Server-Sent Events back. That is the entire surface the model
//! layer needs.
//!
//! It is not a general HTTP client and must not become one. No TLS, no
//! redirects, no connection pool, no proxies. If something ever needs to talk
//! to a remote host over TLS -- cloud escalation, the builder's package archive
//! -- that belongs behind a feature flag with a real client, not here.

#![forbid(unsafe_op_in_unsafe_fn)]

mod sse;

pub use sse::{SseEvent, SseReader};

use std::fmt;
use std::io::{BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

#[derive(Debug)]
pub enum HttpError {
    Connect {
        addr: String,
        source: std::io::Error,
    },
    Io(std::io::Error),
    /// The response did not parse as HTTP/1.1.
    Malformed(String),
    /// A 4xx or 5xx, with whatever body came back for diagnosis.
    Status {
        code: u16,
        body: String,
    },
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HttpError::Connect { addr, source } => {
                write!(f, "cannot connect to {addr}: {source}")
            }
            HttpError::Io(e) => write!(f, "{e}"),
            HttpError::Malformed(what) => write!(f, "malformed HTTP response: {what}"),
            HttpError::Status { code, body } => {
                let trimmed = body.trim();
                if trimmed.is_empty() {
                    write!(f, "HTTP {code}")
                } else {
                    // Cap it: an HTML error page in a log line helps nobody.
                    let short: String = trimmed.chars().take(300).collect();
                    write!(f, "HTTP {code}: {short}")
                }
            }
        }
    }
}

impl std::error::Error for HttpError {}

impl From<std::io::Error> for HttpError {
    fn from(e: std::io::Error) -> Self {
        HttpError::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, HttpError>;

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct Client {
    host: String,
    port: u16,
    timeout: Duration,
}

impl Client {
    pub fn new(host: impl Into<String>, port: u16, timeout: Duration) -> Client {
        Client {
            host: host.into(),
            port,
            timeout,
        }
    }

    fn addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    fn connect(&self) -> Result<TcpStream> {
        let addr = self.addr();
        let stream = TcpStream::connect(&addr).map_err(|source| HttpError::Connect {
            addr: addr.clone(),
            source,
        })?;
        stream.set_read_timeout(Some(self.timeout))?;
        stream.set_write_timeout(Some(self.timeout))?;
        // Small JSON bodies: Nagle would add latency for no benefit.
        let _ = stream.set_nodelay(true);
        Ok(stream)
    }

    fn request_bytes(&self, method: &str, path: &str, body: &str, stream_sse: bool) -> Vec<u8> {
        let accept = if stream_sse {
            "text/event-stream"
        } else {
            "application/json"
        };
        // Connection: close on unary requests means the server signals the end
        // of the body by closing, so a response with neither Content-Length nor
        // chunked encoding still terminates.
        let connection = if stream_sse { "keep-alive" } else { "close" };
        format!(
            "{method} {path} HTTP/1.1\r\n\
             Host: {host}\r\n\
             Accept: {accept}\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {len}\r\n\
             Connection: {connection}\r\n\
             \r\n\
             {body}",
            host = self.host,
            len = body.len(),
        )
        .into_bytes()
    }

    /// POST a JSON body, read the whole response.
    pub fn post_json(&self, path: &str, body: &str) -> Result<Response> {
        let mut stream = self.connect()?;
        stream.write_all(&self.request_bytes("POST", path, body, false))?;
        stream.flush()?;

        let mut reader = BufReader::new(stream);
        let (status, headers) = read_head(&mut reader)?;
        let body = read_body(&mut reader, &headers)?;
        let response = Response {
            status,
            headers,
            body,
        };

        if !(200..300).contains(&response.status) {
            return Err(HttpError::Status {
                code: response.status,
                body: response.body,
            });
        }
        Ok(response)
    }

    pub fn get(&self, path: &str) -> Result<Response> {
        let mut stream = self.connect()?;
        stream.write_all(&self.request_bytes("GET", path, "", false))?;
        stream.flush()?;

        let mut reader = BufReader::new(stream);
        let (status, headers) = read_head(&mut reader)?;
        let body = read_body(&mut reader, &headers)?;
        let response = Response {
            status,
            headers,
            body,
        };

        if !(200..300).contains(&response.status) {
            return Err(HttpError::Status {
                code: response.status,
                body: response.body,
            });
        }
        Ok(response)
    }

    /// POST a JSON body and stream Server-Sent Events back.
    ///
    /// Returns as soon as the headers are in, so the caller sees the first
    /// token without waiting for generation to finish.
    pub fn post_sse(&self, path: &str, body: &str) -> Result<SseReader<BufReader<TcpStream>>> {
        let mut stream = self.connect()?;
        stream.write_all(&self.request_bytes("POST", path, body, true))?;
        stream.flush()?;

        let mut reader = BufReader::new(stream);
        let (status, _headers) = read_head(&mut reader)?;
        if !(200..300).contains(&status) {
            // Error bodies are small; read what is there for the message.
            let mut body = String::new();
            let _ = reader.read_to_string(&mut body);
            return Err(HttpError::Status { code: status, body });
        }
        Ok(SseReader::new(reader))
    }

    /// Is anything listening? Used by `omni doctor` and by the shell
    /// integration, which must fall through silently when the answer is no.
    pub fn reachable(&self) -> bool {
        TcpStream::connect(self.addr()).is_ok()
    }
}

/// Read the status line and headers. Leaves the reader positioned at the body.
fn read_head<R: Read>(reader: &mut BufReader<R>) -> Result<(u16, Vec<(String, String)>)> {
    let status_line = read_line(reader)?;
    let mut parts = status_line.splitn(3, ' ');
    let version = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/1.") {
        return Err(HttpError::Malformed(format!(
            "bad status line: {status_line:?}"
        )));
    }
    let status: u16 = parts
        .next()
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| HttpError::Malformed(format!("bad status code in {status_line:?}")))?;

    let mut headers = Vec::new();
    loop {
        let line = read_line(reader)?;
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
        // A header line with no colon is malformed; skipping it is kinder than
        // failing the whole response over a stray line.
    }
    Ok((status, headers))
}

/// Read one CRLF-terminated line, without the terminator.
fn read_line<R: Read>(reader: &mut BufReader<R>) -> Result<String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let read = reader.read(&mut byte)?;
        if read == 0 {
            if line.is_empty() {
                return Err(HttpError::Malformed("connection closed mid-header".into()));
            }
            break;
        }
        if byte[0] == b'\n' {
            break;
        }
        line.push(byte[0]);
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    String::from_utf8(line).map_err(|e| HttpError::Malformed(format!("non-UTF-8 header: {e}")))
}

fn read_body<R: Read>(reader: &mut BufReader<R>, headers: &[(String, String)]) -> Result<String> {
    let chunked = headers
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("transfer-encoding") && v.contains("chunked"));

    if chunked {
        return read_chunked(reader);
    }

    let length = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse::<usize>().ok());

    let mut body = Vec::new();
    match length {
        Some(n) => {
            body.resize(n, 0);
            reader.read_exact(&mut body)?;
        }
        // No length and no chunking: the body runs to end of stream. Legal
        // under Connection: close, which is what unary requests send.
        None => {
            reader.read_to_end(&mut body)?;
        }
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

fn read_chunked<R: Read>(reader: &mut BufReader<R>) -> Result<String> {
    let mut body = Vec::new();
    loop {
        let size_line = read_line(reader)?;
        // Chunk extensions come after a ';' and are ignored.
        let hex = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(hex, 16)
            .map_err(|_| HttpError::Malformed(format!("bad chunk size: {size_line:?}")))?;
        if size == 0 {
            // Trailers, then the final blank line.
            while !read_line(reader)?.is_empty() {}
            break;
        }
        let mut chunk = vec![0u8; size];
        reader.read_exact(&mut chunk)?;
        body.extend_from_slice(&chunk);
        // Each chunk is followed by CRLF.
        let _ = read_line(reader)?;
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

#[cfg(test)]
mod tests;
