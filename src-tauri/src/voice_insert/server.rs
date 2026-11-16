//! Voice-insert localhost HTTP server (Phase 1).
//!
//! Hand-rolled HTTP/1.1 over `std::net` parsed with `httparse` — the only
//! new dependency. One request per connection (`Connection: close`, no
//! keep-alive), bound to 127.0.0.1 only. The request core
//! ([`handle_request`]) is decoupled from Tauri via the [`InsertDeps`] trait
//! so it is unit/integration-testable without an app.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
use std::time::Duration;

use log::{debug, error, info, warn};
use parking_lot::RwLock;
use serde_json::json;

use super::registry::{RegistryError, VoiceInsertRegistry};

/// Reject /insert bodies larger than this (server-side guard; the API
/// contract is "short tokens", not file upload).
const MAX_BODY_BYTES: usize = 4096;

/// Upper bound on bytes read from the socket per request. Any sane request
/// for this API is far below it; the cap exists so a hostile Content-Length
/// cannot make the connection thread allocate unbounded memory.
const MAX_REQUEST_BYTES: usize = 64 * 1024;

/// API version reported by GET /health.
pub const API_VERSION: u32 = 1;

/// Socket read/write timeout: a half-open local socket must not park a
/// connection thread forever.
const SOCKET_TIMEOUT: Duration = Duration::from_secs(10);

/// Request core, decoupled from Tauri. Implemented by the app-side adapter
/// (coordinator state + registry) and by test fakes.
pub trait InsertDeps {
    /// True only while the coordinator is in `AppState::Recording`. The
    /// server checks this BEFORE `record_token` on every /insert, so bypass
    /// routes that leave the registry buffer unfinalized (cancel,
    /// coordinator timeouts, panic recovery, USB cycle) still answer 409.
    fn is_recording(&self) -> bool;
    /// Record a token against the active recording.
    /// `Err(RegistryError::NotRecording)` maps to HTTP 409,
    /// `Err(RegistryError::BufferFull)` to HTTP 413.
    fn record_token(&self, text: &str) -> Result<(), RegistryError>;
}

/// Extract and verify the bearer token from an Authorization header value.
/// An empty expected token never authenticates (server misconfigured → deny).
fn bearer_ok(auth_header: Option<&str>, expected_token: &str) -> bool {
    if expected_token.is_empty() {
        return false;
    }
    match auth_header {
        Some(header) => header
            .strip_prefix("Bearer ")
            .map(|tok| tok.trim() == expected_token)
            .unwrap_or(false),
        None => false,
    }
}

fn error_body(code: &str) -> String {
    json!({ "error": code }).to_string()
}

/// Handle one parsed HTTP request. Pure with respect to `deps` — no socket,
/// no Tauri. Returns (status, JSON body).
pub fn handle_request(
    method: &str,
    path: &str,
    auth_header: Option<&str>,
    body: &[u8],
    deps: &dyn InsertDeps,
    expected_token: &str,
) -> (u16, String) {
    match path {
        "/health" => {
            if method != "GET" {
                return (405, error_body("method_not_allowed"));
            }
            (
                200,
                json!({ "ok": true, "version": API_VERSION }).to_string(),
            )
        }
        "/state" => {
            if method != "GET" {
                return (405, error_body("method_not_allowed"));
            }
            if !bearer_ok(auth_header, expected_token) {
                return (401, error_body("unauthorized"));
            }
            let state = if deps.is_recording() {
                "recording"
            } else {
                "idle"
            };
            (200, json!({ "state": state }).to_string())
        }
        "/insert" => {
            if method != "POST" {
                return (405, error_body("method_not_allowed"));
            }
            if !bearer_ok(auth_header, expected_token) {
                return (401, error_body("unauthorized"));
            }
            if body.len() > MAX_BODY_BYTES {
                return (400, error_body("body_too_large"));
            }
            let parsed: serde_json::Value = match serde_json::from_slice(body) {
                Ok(v) => v,
                Err(_) => return (400, error_body("invalid_json")),
            };
            let text = match parsed.get("text").and_then(|v| v.as_str()) {
                Some(t) => t,
                None => return (400, error_body("missing_text")),
            };
            // Strip/trim server-side so token anchors and the final splice
            // never see surrounding whitespace or a raw `[[...]]`-less blank.
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return (400, error_body("empty_text"));
            }
            // Gate on the coordinator's live recording state FIRST. The
            // registry buffer can still be unfinalized after bypass routes
            // (cancel, coordinator timeouts, panic recovery, USB cycle) —
            // record_token alone cannot detect those; only the coordinator
            // state can.
            if !deps.is_recording() {
                return (409, json!({ "error": "not_recording" }).to_string());
            }
            match deps.record_token(trimmed) {
                Ok(()) => (200, json!({ "ok": true }).to_string()),
                Err(RegistryError::NotRecording) => {
                    (409, json!({ "error": "not_recording" }).to_string())
                }
                Err(RegistryError::BufferFull) => {
                    (413, json!({ "error": "buffer_full" }).to_string())
                }
            }
        }
        _ => (404, error_body("not_found")),
    }
}

/// Serialize an HTTP/1.1 response with `Connection: close`.
fn http_response(status: u16, body: &str) -> Vec<u8> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        _ => "OK",
    };
    format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        reason,
        body.len(),
        body
    )
    .into_bytes()
}

/// Parsed request tuple: (method, path, authorization header, body).
type ParsedRequest = (String, String, Option<String>, Vec<u8>);

/// Read outcome: a parsed request, `None` on premature EOF, or a pre-built
/// error response to write back (501 chunked encoding, 400 oversized).
type ReadOutcome = Result<Option<ParsedRequest>, Vec<u8>>;

/// Read one request from the socket: headers via httparse (loop-fed), then
/// exactly Content-Length body bytes.
fn read_request(stream: &mut TcpStream) -> ReadOutcome {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];

    // Feed bytes until httparse completes the headers; every branch of the
    // match below returns, so this loop only continues on Partial parses.
    loop {
        let n = match stream.read(&mut chunk) {
            Ok(0) => {
                debug!("voice insert: connection closed before headers completed");
                return Ok(None);
            }
            Ok(n) => n,
            Err(e) => {
                debug!("voice insert: socket read error: {}", e);
                return Ok(None);
            }
        };
        buf.extend_from_slice(&chunk[..n]);

        let mut headers = [httparse::EMPTY_HEADER; 32];
        let mut request = httparse::Request::new(&mut headers);
        match request.parse(&buf) {
            Ok(httparse::Status::Complete(header_size)) => {
                let method = request.method.unwrap_or("").to_string();
                let path = request.path.unwrap_or("").to_string();
                let mut content_length: usize = 0;
                for header in request.headers.iter() {
                    let name = header.name.to_ascii_lowercase();
                    if name == "transfer-encoding" {
                        debug!("voice insert: rejected Transfer-Encoding request");
                        return Err(http_response(
                            501,
                            r#"{"error":"transfer_encoding_unsupported"}"#,
                        ));
                    } else if name == "content-length" {
                        let value = String::from_utf8_lossy(header.value);
                        content_length = value.trim().parse().unwrap_or(0);
                    }
                }
                let auth_header = request
                    .headers
                    .iter()
                    .find(|h| h.name.eq_ignore_ascii_case("authorization"))
                    .map(|h| String::from_utf8_lossy(h.value).into_owned());

                if content_length > MAX_REQUEST_BYTES {
                    warn!(
                        "voice insert: Content-Length {} exceeds cap, rejecting",
                        content_length
                    );
                    return Err(http_response(400, r#"{"error":"body_too_large"}"#));
                }

                // Read exactly Content-Length body bytes.
                let mut body = buf[header_size..].to_vec();
                while body.len() < content_length {
                    let n = match stream.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(e) => {
                            debug!("voice insert: body read error: {}", e);
                            break;
                        }
                    };
                    body.extend_from_slice(&chunk[..n]);
                }
                body.truncate(content_length);
                return Ok(Some((method, path, auth_header, body)));
            }
            Ok(httparse::Status::Partial) => {
                if buf.len() > MAX_REQUEST_BYTES {
                    return Err(http_response(400, r#"{"error":"request_too_large"}"#));
                }
                continue;
            }
            Err(e) => {
                debug!("voice insert: unparseable request: {}", e);
                return Err(http_response(400, r#"{"error":"invalid_request"}"#));
            }
        }
    }
}

/// Serve one connection: read a single request, dispatch, respond, close.
fn handle_connection(stream: &mut TcpStream, deps: &dyn InsertDeps, token: &Arc<RwLock<String>>) {
    // Socket hygiene: a half-open local socket must not park this thread
    // forever. Best-effort — failures only lose the timeout, not the request.
    let _ = stream.set_read_timeout(Some(SOCKET_TIMEOUT));
    let _ = stream.set_write_timeout(Some(SOCKET_TIMEOUT));

    let (method, path, auth_header, body) = match read_request(stream) {
        Ok(Some(request)) => request,
        Ok(None) => return,
        Err(prebuilt) => {
            let _ = stream.write_all(&prebuilt);
            return;
        }
    };

    let expected_token = token.read().clone();

    // Per-request handling is panic-isolated: a bug in the splice path must
    // not wedge the accept loop, it answers 500 and the connection closes.
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        handle_request(
            &method,
            &path,
            auth_header.as_deref(),
            &body,
            deps,
            &expected_token,
        )
    }));
    let (status, body) = match outcome {
        Ok(result) => result,
        Err(_) => {
            error!(
                "voice insert: request handler panicked ({} {})",
                method, path
            );
            (500, r#"{"error":"internal"}"#.to_string())
        }
    };

    debug!("voice insert: {} {} -> {}", method, path, status);
    let _ = stream.write_all(&http_response(status, &body));
    let _ = stream.flush();
    // Connection: close — drop closes the socket.
}

/// App-side adapter: coordinator state for the recording gate, managed
/// registry for token recording.
struct AppInsertDeps {
    app: tauri::AppHandle,
}

impl InsertDeps for AppInsertDeps {
    fn is_recording(&self) -> bool {
        use crate::transcription_coordinator::{AppState, TranscriptionCoordinator};
        // Gate on Recording only: Confirming/UsbCycling/Processing all
        // answer 409 so inserts can never land outside a live recording.
        matches!(
            tauri::Manager::state::<TranscriptionCoordinator>(&self.app).get_state(),
            AppState::Recording { .. }
        )
    }

    fn record_token(&self, text: &str) -> Result<(), RegistryError> {
        match tauri::Manager::try_state::<Arc<VoiceInsertRegistry>>(&self.app) {
            Some(registry) => registry.record_token(text),
            None => Err(RegistryError::NotRecording),
        }
    }
}

/// Bind 127.0.0.1:`port` and start the accept loop on a dedicated
/// std::thread (mirrors the signal_handle.rs thread pattern). Port 0 binds
/// an ephemeral port (used by tests). Returns the bound listener error to
/// the caller for startup logging — a port conflict must never fail app
/// boot, so `start_server` logs and continues without the server.
pub fn start_server(app: tauri::AppHandle, port: u16, token: Arc<RwLock<String>>) {
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => listener,
        Err(e) => {
            error!(
                "Voice insert server failed to bind 127.0.0.1:{} — continuing without it ({}). \
                 Change the port in Settings → Integrations and restart.",
                port, e
            );
            return;
        }
    };
    let addr = listener
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| format!("127.0.0.1:{}", port));
    info!("Voice insert server listening on http://{}", addr);

    let deps: Arc<dyn InsertDeps + Send + Sync> = Arc::new(AppInsertDeps { app });
    std::thread::Builder::new()
        .name("voice-insert-server".to_string())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(mut stream) => {
                        let deps = Arc::clone(&deps);
                        let token = Arc::clone(&token);
                        // One short-lived thread per connection: a slow or
                        // wedged client cannot block the accept loop. The
                        // socket is closed when the handler returns.
                        std::thread::spawn(move || {
                            handle_connection(&mut stream, deps.as_ref(), &token);
                        });
                    }
                    Err(e) => {
                        debug!("voice insert: accept error: {}", e);
                    }
                }
            }
            debug!("voice insert: accept loop ended");
        })
        .map_err(|e| warn!("voice insert: failed to spawn accept thread: {}", e))
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Shutdown;
    use std::time::Duration;

    /// Test fake: recording flag + recorded-token log.
    struct FakeDeps {
        recording: bool,
        recorded: Arc<RwLock<Vec<String>>>,
    }

    impl InsertDeps for FakeDeps {
        fn is_recording(&self) -> bool {
            self.recording
        }

        fn record_token(&self, text: &str) -> Result<(), RegistryError> {
            if !self.recording {
                return Err(RegistryError::NotRecording);
            }
            self.recorded.write().push(text.to_string());
            Ok(())
        }
    }

    /// Registry-backed deps: forwards to a real registry so buffer-full and
    /// gate interactions are exercised through handle_request end-to-end.
    struct RegistryBackedDeps {
        recording: bool,
        registry: Arc<VoiceInsertRegistry>,
    }

    impl InsertDeps for RegistryBackedDeps {
        fn is_recording(&self) -> bool {
            self.recording
        }

        fn record_token(&self, text: &str) -> Result<(), RegistryError> {
            self.registry.record_token(text)
        }
    }

    fn fake(recording: bool) -> (FakeDeps, Arc<RwLock<Vec<String>>>) {
        let recorded = Arc::new(RwLock::new(Vec::new()));
        (
            FakeDeps {
                recording,
                recorded: Arc::clone(&recorded),
            },
            recorded,
        )
    }

    const TOKEN: &str = "abc123";

    // ── handle_request unit tests ─────────────────────────────────────

    #[test]
    fn health_needs_no_auth() {
        let (deps, _) = fake(false);
        let (status, body) = handle_request("GET", "/health", None, b"", &deps, TOKEN);
        assert_eq!(status, 200);
        assert_eq!(body, r#"{"ok":true,"version":1}"#);
    }

    #[test]
    fn insert_ok_when_recording_and_trims_text() {
        let (deps, recorded) = fake(true);
        let (status, body) = handle_request(
            "POST",
            "/insert",
            Some(&format!("Bearer {TOKEN}")),
            br#"{"text": "  [[Note A]]  "}"#,
            &deps,
            TOKEN,
        );
        assert_eq!(status, 200);
        assert_eq!(body, r#"{"ok":true}"#);
        assert_eq!(recorded.read().as_slice(), ["[[Note A]]"]);
    }

    #[test]
    fn insert_while_idle_is_conflict() {
        // Exercises the handle_request gate: deps report is_recording()==false
        // and the 409 comes from the gate, never from record_token (see
        // insert_gates_on_is_recording_before_record_token for the proof).
        let (deps, recorded) = fake(false);
        let (status, body) = handle_request(
            "POST",
            "/insert",
            Some(&format!("Bearer {TOKEN}")),
            br#"{"text":"x"}"#,
            &deps,
            TOKEN,
        );
        assert_eq!(status, 409);
        assert_eq!(body, r#"{"error":"not_recording"}"#);
        assert!(recorded.read().is_empty());
    }

    #[test]
    fn insert_gates_on_is_recording_before_record_token() {
        // record_token always succeeds, but is_recording()==false: only the
        // explicit gate can produce the 409. Covers bypass routes (cancel,
        // coordinator timeouts, panic recovery, USB cycle) that leave the
        // registry buffer unfinalized.
        struct AlwaysOkDeps;
        impl InsertDeps for AlwaysOkDeps {
            fn is_recording(&self) -> bool {
                false
            }
            fn record_token(&self, _text: &str) -> Result<(), RegistryError> {
                Ok(())
            }
        }
        let (status, body) = handle_request(
            "POST",
            "/insert",
            Some(&format!("Bearer {TOKEN}")),
            br#"{"text":"x"}"#,
            &AlwaysOkDeps,
            TOKEN,
        );
        assert_eq!(status, 409);
        assert_eq!(body, r#"{"error":"not_recording"}"#);
    }

    #[test]
    fn insert_returns_413_when_buffer_full() {
        let registry = Arc::new(VoiceInsertRegistry::new());
        registry.begin("s1".to_string());
        let deps = RegistryBackedDeps {
            recording: true,
            registry,
        };
        let auth = format!("Bearer {TOKEN}");
        let body: &[u8] = br#"{"text":"t"}"#;
        for i in 0..super::super::registry::MAX_TOKENS_PER_SESSION {
            let (status, _) = handle_request("POST", "/insert", Some(&auth), body, &deps, TOKEN);
            assert_eq!(status, 200, "token {i} should still fit");
        }
        // The 257th insert hits the cap → 413 buffer_full.
        let (status, err_body) = handle_request("POST", "/insert", Some(&auth), body, &deps, TOKEN);
        assert_eq!(status, 413);
        assert_eq!(err_body, r#"{"error":"buffer_full"}"#);
    }

    #[test]
    fn insert_rejects_bad_or_missing_bearer() {
        let (deps, _) = fake(true);
        let (status, _) = handle_request(
            "POST",
            "/insert",
            Some("Bearer wrong"),
            br#"{"text":"x"}"#,
            &deps,
            TOKEN,
        );
        assert_eq!(status, 401);
        let (status, _) = handle_request("POST", "/insert", None, br#"{"text":"x"}"#, &deps, TOKEN);
        assert_eq!(status, 401);
        // Empty configured token denies everything.
        let (status, _) = handle_request(
            "POST",
            "/insert",
            Some("Bearer abc123"),
            br#"{"text":"x"}"#,
            &deps,
            "",
        );
        assert_eq!(status, 401);
    }

    #[test]
    fn insert_rejects_bad_json_empty_text_and_oversized_body() {
        let (deps, _) = fake(true);
        let (status, _) = handle_request(
            "POST",
            "/insert",
            Some(&format!("Bearer {TOKEN}")),
            b"not json",
            &deps,
            TOKEN,
        );
        assert_eq!(status, 400);
        let (status, _) = handle_request(
            "POST",
            "/insert",
            Some(&format!("Bearer {TOKEN}")),
            br#"{"text":"   "}"#,
            &deps,
            TOKEN,
        );
        assert_eq!(status, 400);
        let (status, _) = handle_request(
            "POST",
            "/insert",
            Some(&format!("Bearer {TOKEN}")),
            br#"{"nope":1}"#,
            &deps,
            TOKEN,
        );
        assert_eq!(status, 400);
        let big = vec![b'a'; MAX_BODY_BYTES + 1];
        let (status, _) = handle_request(
            "POST",
            "/insert",
            Some(&format!("Bearer {TOKEN}")),
            &big,
            &deps,
            TOKEN,
        );
        assert_eq!(status, 400);
    }

    #[test]
    fn unknown_path_is_404_and_wrong_method_is_405() {
        let (deps, _) = fake(true);
        let (status, _) = handle_request(
            "GET",
            "/nope",
            Some(&format!("Bearer {TOKEN}")),
            b"",
            &deps,
            TOKEN,
        );
        assert_eq!(status, 404);
        let (status, _) = handle_request(
            "GET",
            "/insert",
            Some(&format!("Bearer {TOKEN}")),
            b"",
            &deps,
            TOKEN,
        );
        assert_eq!(status, 405);
        let (status, _) = handle_request(
            "POST",
            "/state",
            Some(&format!("Bearer {TOKEN}")),
            b"",
            &deps,
            TOKEN,
        );
        assert_eq!(status, 405);
    }

    #[test]
    fn state_endpoint_mirrors_recording_gate() {
        let (deps, _) = fake(true);
        let (status, body) = handle_request(
            "GET",
            "/state",
            Some(&format!("Bearer {TOKEN}")),
            b"",
            &deps,
            TOKEN,
        );
        assert_eq!(status, 200);
        assert_eq!(body, r#"{"state":"recording"}"#);

        let (idle_deps, _) = fake(false);
        let (status, body) = handle_request(
            "GET",
            "/state",
            Some(&format!("Bearer {TOKEN}")),
            b"",
            &idle_deps,
            TOKEN,
        );
        assert_eq!(status, 200);
        assert_eq!(body, r#"{"state":"idle"}"#);
    }

    // ── socket-level integration tests ────────────────────────────────

    /// Bind 127.0.0.1:0, run the accept loop on a thread, return the port.
    fn spawn_test_server(deps: Arc<dyn InsertDeps + Send + Sync>) -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let token = Arc::new(RwLock::new(TOKEN.to_string()));
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let deps = Arc::clone(&deps);
                let token = Arc::clone(&token);
                std::thread::spawn(move || {
                    let mut stream = stream;
                    handle_connection(&mut stream, deps.as_ref(), &token);
                });
            }
        });
        port
    }

    /// Send one raw HTTP/1.1 request, read the full response (to EOF), and
    /// assert the status line contains `expected_status`.
    fn drive_request(port: u16, raw: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(raw.as_bytes()).unwrap();
        let mut response = Vec::new();
        // Read to EOF: the server must close the connection after one
        // response (Connection: close, no keep-alive).
        let _ = stream.read_to_end(&mut response);
        String::from_utf8_lossy(&response).into_owned()
    }

    #[test]
    fn socket_drives_all_status_codes_and_closes() {
        let (fake_deps, recorded) = fake(true);
        let deps: Arc<dyn InsertDeps + Send + Sync> = Arc::new(fake_deps);
        let port = spawn_test_server(Arc::clone(&deps));

        let auth = format!("Authorization: Bearer {TOKEN}\r\n");
        let insert_body = r#"{"text":"[[Note A]]"}"#;
        let insert_request = format!(
            "POST /insert HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n{auth}\r\n{insert_body}",
            insert_body.len()
        );
        let cases: Vec<(String, &str)> = vec![
            (
                "GET /health HTTP/1.1\r\nHost: x\r\n\r\n".to_string(),
                "HTTP/1.1 200 OK",
            ),
            (
                "GET /state HTTP/1.1\r\nHost: x\r\n\r\n".to_string(),
                "HTTP/1.1 401 Unauthorized",
            ),
            (
                format!("GET /state HTTP/1.1\r\nHost: x\r\n{auth}\r\n"),
                "HTTP/1.1 200 OK",
            ),
            (
                format!("GET /nope HTTP/1.1\r\nHost: x\r\n{auth}\r\n"),
                "HTTP/1.1 404 Not Found",
            ),
            (
                format!("GET /insert HTTP/1.1\r\nHost: x\r\n{auth}\r\n"),
                "HTTP/1.1 405 Method Not Allowed",
            ),
            (insert_request, "HTTP/1.1 200 OK"),
        ];
        for (raw, expected) in cases {
            let response = drive_request(port, &raw);
            assert!(
                response.starts_with(expected),
                "expected '{expected}' for {raw:?}, got: {response:?}"
            );
            assert!(response.contains("Connection: close"));
        }
        assert_eq!(recorded.read().as_slice(), ["[[Note A]]"]);

        // Not-recording → 409 over the wire: flip the fake to idle.
        let (idle_deps, _) = fake(false);
        let idle_port = spawn_test_server(Arc::new(idle_deps) as Arc<dyn InsertDeps + Send + Sync>);
        let idle_body = r#"{"text":"x"}"#;
        let response = drive_request(
            idle_port,
            &format!(
                "POST /insert HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n{auth}\r\n{idle_body}",
                idle_body.len()
            ),
        );
        assert!(response.starts_with("HTTP/1.1 409 Conflict"));

        // Chunked transfer-encoding → 501 over the wire.
        let te_port = spawn_test_server(Arc::clone(&deps));
        let response = drive_request(
            te_port,
            &format!("POST /insert HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n{auth}\r\n0\r\n\r\n"),
        );
        assert!(response.starts_with("HTTP/1.1 501 Not Implemented"));
    }

    #[test]
    fn socket_connection_closes_after_response() {
        let (deps, _) = fake(true);
        let port = spawn_test_server(Arc::new(deps) as Arc<dyn InsertDeps + Send + Sync>);
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .write_all(b"GET /health HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response);
        assert!(!response.is_empty(), "no response received");
        // read_to_end returning means the server half-closed: one request,
        // one response, connection closed.
        let _ = stream.shutdown(Shutdown::Both);
    }
}
