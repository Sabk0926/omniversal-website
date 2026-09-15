//! Tests run against a real TcpListener on 127.0.0.1 serving canned bytes.
//! No network, no mocking framework, and it exercises the actual socket path.

use super::*;
use std::io::BufReader;
use std::net::TcpListener;
use std::thread;

/// Serve one connection with `response`, return the port. The request bytes
/// are discarded; tests that care assert on them via `serve_capturing`.
fn serve(response: &'static str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    port
}

fn client(port: u16) -> Client {
    Client::new("127.0.0.1", port, Duration::from_secs(5))
}

#[test]
fn reads_a_content_length_body() {
    let port = serve(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 17\r\n\r\n{\"ok\":true,\"n\":1}",
    );
    let response = client(port).post_json("/v1/chat", "{}").unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.body, "{\"ok\":true,\"n\":1}");
    assert_eq!(response.header("content-type"), Some("application/json"));
}

#[test]
fn header_lookup_is_case_insensitive() {
    let port = serve("HTTP/1.1 200 OK\r\nX-Model: qwen\r\nContent-Length: 2\r\n\r\nhi");
    let response = client(port).post_json("/x", "{}").unwrap();
    assert_eq!(response.header("x-model"), Some("qwen"));
    assert_eq!(response.header("X-MODEL"), Some("qwen"));
}

#[test]
fn reads_a_chunked_body() {
    // llama.cpp uses chunked for some endpoints, so this path is not optional.
    let port = serve(
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n",
    );
    let response = client(port).post_json("/x", "{}").unwrap();
    assert_eq!(response.body, "hello world");
}

#[test]
fn chunk_extensions_are_ignored() {
    let port =
        serve("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3;foo=bar\r\nabc\r\n0\r\n\r\n");
    assert_eq!(client(port).post_json("/x", "{}").unwrap().body, "abc");
}

#[test]
fn body_without_length_runs_to_end_of_stream() {
    // Legal under Connection: close, which is what unary requests send.
    let port = serve("HTTP/1.1 200 OK\r\n\r\ntrailing body");
    assert_eq!(
        client(port).post_json("/x", "{}").unwrap().body,
        "trailing body"
    );
}

#[test]
fn error_status_carries_the_body_for_diagnosis() {
    let port = serve(
        "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 21\r\n\r\n{\"error\":\"no model\"}\n",
    );
    let err = client(port).post_json("/x", "{}").unwrap_err();
    match &err {
        HttpError::Status { code, body } => {
            assert_eq!(*code, 503);
            assert!(body.contains("no model"), "body preserved: {body}");
        }
        other => panic!("expected Status, got {other:?}"),
    }
    assert!(err.to_string().contains("503"));
}

#[test]
fn connect_failure_names_the_address() {
    // Port 1 on loopback: nothing listens, and binding it needs root.
    let err = Client::new("127.0.0.1", 1, Duration::from_millis(500))
        .post_json("/x", "{}")
        .unwrap_err();
    assert!(matches!(err, HttpError::Connect { .. }));
    assert!(
        err.to_string().contains("127.0.0.1:1"),
        "names the addr: {err}"
    );
}

#[test]
fn reachable_is_false_when_nothing_listens() {
    assert!(!Client::new("127.0.0.1", 1, Duration::from_millis(500)).reachable());
}

#[test]
fn garbage_is_rejected_as_malformed() {
    let port = serve("this is not http\r\n\r\n");
    let err = client(port).post_json("/x", "{}").unwrap_err();
    assert!(matches!(err, HttpError::Malformed(_)), "got {err:?}");
}

// --- SSE ------------------------------------------------------------------

fn events(raw: &str) -> Vec<SseEvent> {
    SseReader::new(BufReader::new(raw.as_bytes()))
        .map(|e| e.expect("event"))
        .collect()
}

#[test]
fn parses_a_token_stream() {
    let parsed = events("data: {\"t\":\"he\"}\n\ndata: {\"t\":\"llo\"}\n\ndata: [DONE]\n\n");
    assert_eq!(parsed.len(), 2, "[DONE] is a terminator, not an event");
    assert_eq!(parsed[0].data, "{\"t\":\"he\"}");
    assert_eq!(parsed[1].data, "{\"t\":\"llo\"}");
}

#[test]
fn stops_at_done_and_ignores_anything_after() {
    let parsed = events("data: one\n\ndata: [DONE]\n\ndata: two\n\n");
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].data, "one");
}

#[test]
fn multiline_data_joins_with_newline() {
    let parsed = events("data: line one\ndata: line two\n\n");
    assert_eq!(parsed[0].data, "line one\nline two");
}

#[test]
fn comments_are_keepalives_and_are_skipped() {
    let parsed = events(": ping\n\ndata: real\n\n");
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].data, "real");
}

#[test]
fn named_events_are_captured() {
    let parsed = events("event: error\ndata: boom\n\n");
    assert_eq!(parsed[0].name.as_deref(), Some("error"));
    assert_eq!(parsed[0].data, "boom");
}

#[test]
fn unknown_fields_do_not_break_the_stream() {
    let parsed = events("id: 7\nretry: 100\ndata: still works\n\n");
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].data, "still works");
}

#[test]
fn truncated_stream_yields_what_arrived() {
    // A dropped connection mid-generation must not discard tokens already sent.
    let parsed = events("data: partial");
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].data, "partial");
}

#[test]
fn crlf_line_endings_parse() {
    let parsed = events("data: windows\r\n\r\n");
    assert_eq!(parsed[0].data, "windows");
}

#[test]
fn sse_over_a_real_socket() {
    let port = serve(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\ndata: a\n\ndata: b\n\ndata: [DONE]\n\n",
    );
    let collected: Vec<String> = client(port)
        .post_sse("/v1/chat", "{}")
        .unwrap()
        .map(|e| e.unwrap().data)
        .collect();
    assert_eq!(collected, vec!["a", "b"]);
}

#[test]
fn sse_surfaces_an_error_status_instead_of_streaming() {
    let port = serve("HTTP/1.1 400 Bad Request\r\nContent-Length: 9\r\n\r\nbad model");
    let err = client(port).post_sse("/v1/chat", "{}").unwrap_err();
    match err {
        HttpError::Status { code, body } => {
            assert_eq!(code, 400);
            assert!(body.contains("bad model"));
        }
        other => panic!("expected Status, got {other:?}"),
    }
}
