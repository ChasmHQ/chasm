use super::*;
use axum::{
    body::{to_bytes, Body},
    http::{header, Request},
};
use std::time::Duration;
use tower::ServiceExt;

struct Harness {
    manager: Arc<Manager>,
    app: Router,
    cookie: String,
    csrf: String,
    _dir: tempfile::TempDir,
    script: PathBuf,
}
impl Harness {
    async fn new(timeout: Duration) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("mock_worker.py");
        std::fs::write(&script, include_str!("mock_worker.py")).unwrap();
        let mut config = Config::new(dir.path().to_path_buf(), None, None, None);
        config.timeout = timeout;
        config.worker_script = Some(script.clone());
        config.service_dir = dir.path().to_path_buf();
        config.example = dir.path().join("capture.json");
        std::fs::write(&config.example, "{}").unwrap();
        // Keep test execution deterministic without PATH lookup or public downloads.
        config.python = std::env::var_os("CHASM_DEBUGGER_TEST_PYTHON")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .unwrap()
                    .join("debugger")
                    .join(if cfg!(windows) {
                        ".venv/Scripts/python.exe"
                    } else {
                        ".venv/bin/python"
                    })
            });
        let manager = Manager::new(config).await;
        assert!(
            manager.availability.is_none(),
            "mock Python unavailable: {:?}; set CHASM_DEBUGGER_TEST_PYTHON",
            manager.availability
        );
        let app = Router::new().nest("/api/debugger", router(manager.clone()));
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/debugger/capabilities")
                    .header(header::HOST, "localhost:3000")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let body = decode(response).await;
        let csrf = body["csrfToken"].as_str().unwrap().to_string();
        Self {
            manager,
            app,
            cookie,
            csrf,
            _dir: dir,
            script,
        }
    }
    async fn call(&self, method: &str, route: &str, body: Value) -> (StatusCode, Value) {
        let request = Request::builder()
            .method(method)
            .uri(format!("/api/debugger{route}"))
            .header(header::HOST, "localhost:3000")
            .header(header::ORIGIN, "http://localhost:3000")
            .header(header::COOKIE, &self.cookie)
            .header("x-chasm-csrf", &self.csrf)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        (status, decode(response).await)
    }
    async fn start(&self) -> Value {
        let (status, envelope) = self
            .call(
                "POST",
                "/sessions",
                json!({"requestId":"start", "mode":"offline", "captureId":"synthetic"}),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{envelope}");
        assert_eq!(envelope["busy"], true);
        self.idle(envelope["id"].as_str().unwrap()).await
    }
    async fn idle(&self, id: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let (_, envelope) = self
                    .call("GET", &format!("/sessions/{id}"), json!({}))
                    .await;
                if envelope["busy"] == false {
                    return envelope;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("session did not settle")
    }
    async fn cmd(
        &self,
        envelope: &Value,
        id: &str,
        method: &str,
        params: Value,
    ) -> (StatusCode, Value) {
        self.call("POST", &format!("/sessions/{}/commands", envelope["id"].as_str().unwrap()), json!({
            "requestId":id, "generation":envelope["generation"], "expectedRevision":envelope["revision"], "method":method, "params":params,
        })).await
    }
    fn records(&self) -> Vec<Value> {
        std::fs::read_to_string(self.script.with_extension("record"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}
async fn decode(response: Response) -> Value {
    let bytes = to_bytes(response.into_body(), MAX_BODY * 3).await.unwrap();
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| json!({"raw":String::from_utf8_lossy(&bytes)}))
}

#[tokio::test]
async fn bootstrap_and_security_boundaries() {
    let h = Harness::new(Duration::from_secs(5)).await;
    for (host, origin, fetch) in [
        ("evil.test:3000", "http://localhost:3000", "same-origin"),
        ("localhost:3001", "http://localhost:3000", "same-origin"),
        ("localhost:3000", "http://evil.test", "same-origin"),
        ("localhost:3000", "http://127.0.0.1:3000", "same-origin"),
        ("localhost:3000", "null", "same-origin"),
        ("localhost:3000", "http://localhost:3000", "cross-site"),
    ] {
        let response = h
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/debugger/capabilities")
                    .header(header::HOST, host)
                    .header(header::ORIGIN, origin)
                    .header("sec-fetch-site", fetch)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none());
    }
    for (cookie, csrf) in [(false, false), (true, false)] {
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/debugger/resources")
            .header(header::HOST, "localhost:3000");
        if cookie {
            request = request.header(header::COOKIE, &h.cookie);
        }
        if csrf {
            request = request.header("x-chasm-csrf", &h.csrf);
        }
        let response = h
            .app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        // Only the missing-owner case may be renewed by re-opening capabilities.
        let bytes = to_bytes(response.into_body(), MAX_BODY).await.unwrap();
        let code = serde_json::from_slice::<Value>(&bytes).unwrap()["error"]["code"].clone();
        assert_eq!(code == OWNER_EXPIRED, !cookie, "cookie={cookie}");
    }
    let response = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/debugger/capabilities")
                .header(header::HOST, "127.0.0.1:3000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    assert!(
        cookie.contains("HttpOnly")
            && cookie.contains("SameSite=Strict")
            && cookie.contains("Path=/api/debugger")
    );
    assert!(response
        .headers()
        .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .is_none());
    let response = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/debugger/capabilities?token=secret")
                .header(header::HOST, "localhost:3000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let (status, _) = h.call("GET", "/sessions/not-found/events", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST); // No WebSocket upgrade headers.
    h.manager.shutdown().await;
}

#[tokio::test]
async fn async_start_revision_dedupe_results_and_export() {
    let h = Harness::new(Duration::from_secs(5)).await;
    let first = h.start().await;
    assert_eq!(first["state"], "paused");
    assert_eq!(first["revision"], 1);
    assert!(first["snapshot"]["params"]["capture_path"].is_string());
    let (status, _) = h
        .call(
            "POST",
            "/sessions",
            json!({"requestId":"other-start", "mode":"offline", "captureId":"synthetic"}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, accepted) = h
        .cmd(
            &first,
            "evaluate-once",
            "evaluate",
            json!({"expression":"1 + 41"}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(accepted["busy"], true);
    let settled = h.idle(first["id"].as_str().unwrap()).await;
    assert_eq!(settled["snapshot"]["pc"], 1);
    assert_eq!(settled["result"]["evaluation"]["value"], "42");
    let (status, _) = h.cmd(&first, "stale", "mutate", json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = h
        .cmd(
            &first,
            "evaluate-once",
            "evaluate",
            json!({"expression":"1 + 41"}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        h.records()
            .iter()
            .filter(|r| r["method"] == "evaluate")
            .count(),
        1
    );
    let (status, _) = h
        .cmd(
            &settled,
            "evaluate-once",
            "evaluate",
            json!({"expression":"changed"}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = h.cmd(&settled, "export", "export_capture", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let exported = h.idle(first["id"].as_str().unwrap()).await;
    assert_eq!(exported["result"]["capture"], "mock");
    assert_eq!(exported["snapshot"]["pc"], 1);
    let (status, _) = h
        .cmd(
            &exported,
            "mutation-target",
            "mutate",
            json!({"target":"storage", "slot":"0x0", "value":"0x1"}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let mutated = h.idle(first["id"].as_str().unwrap()).await;
    assert!(mutated["error"].is_null());
    assert_eq!(
        h.records()
            .iter()
            .filter(|r| r["method"] == "mutate" && r["params"]["target"] == "storage")
            .count(),
        1
    );
    h.manager.shutdown().await;
}

#[tokio::test]
async fn resource_ownership_paths_and_size() {
    let h = Harness::new(Duration::from_secs(5)).await;
    let (status, resource) = h
        .call(
            "POST",
            "/resources",
            json!({"kind":"capture", "name":"../../browser-path.json", "content":{"steps":[]}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!resource["id"].as_str().unwrap().contains('/'));
    let registered = h.manager.registry.lock().unwrap().resources[resource["id"].as_str().unwrap()]
        .path
        .clone();
    assert!(registered.starts_with(h.manager.resources_dir.as_ref().unwrap().path()));
    for id in ["../../browser-path.json", "C:/secret.json", "/etc/passwd"] {
        let (status, _) = h
            .call(
                "POST",
                "/sessions",
                json!({"requestId":"bad-path", "mode":"offline", "captureId":id}),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
    for target in [
        "../outside.sol",
        "C:/outside.sol",
        "missing.sol",
        "folder\\outside.sol",
        "file.txt",
    ] {
        let (status, _) = h
            .call(
                "POST",
                "/sessions",
                json!({"requestId":"bad-target", "mode":"foundry", "target":target}),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let other = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/debugger/capabilities")
                .header(header::HOST, "localhost:3000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let cookie = other.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let csrf = decode(other).await["csrfToken"]
        .as_str()
        .unwrap()
        .to_string();
    let response = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/debugger/sessions")
                .header(header::HOST, "localhost:3000")
                .header(header::COOKIE, cookie)
                .header("x-chasm-csrf", csrf)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"requestId":"foreign", "mode":"offline", "captureId":resource["id"]})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/debugger/resources")
                .header(header::HOST, "localhost:3000")
                .header(header::COOKIE, &h.cookie)
                .header("x-chasm-csrf", &h.csrf)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(" ".repeat(MAX_BODY + 1)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let e = h.start().await;
    let (status, _) = h
        .cmd(&e, "unsafe", "command", json!({"path":"/tmp/secret"}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    h.manager.shutdown().await;
}

#[tokio::test]
async fn cancellation_remains_responsive_and_kills_process_tree() {
    let h = Harness::new(Duration::from_secs(30)).await;
    let envelope = h.start().await;
    let id = envelope["id"].as_str().unwrap();
    let (status, _) = h
        .cmd(&envelope, "hang", "navigate", json!({"action":"hang"}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let child_file = h.script.with_extension("child");
    let pid = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(pid) = std::fs::read_to_string(&child_file)
                .ok()
                .and_then(|content| content.trim().parse::<u32>().ok())
                .filter(|pid| *pid > 0)
            {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("mock worker did not publish a valid descendant PID");
    let (status, busy) = tokio::time::timeout(
        Duration::from_secs(2),
        h.call("GET", &format!("/sessions/{id}"), json!({})),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(busy["busy"], true);
    let (status, _) = h.cmd(&envelope, "busy", "snapshot", json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, cancelled) = tokio::time::timeout(
        Duration::from_secs(3),
        h.call("DELETE", &format!("/sessions/{id}"), json!({})),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cancelled["state"], "cancelled");
    assert_eq!(cancelled["busy"], false);
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::{
            Foundation::{CloseHandle, STILL_ACTIVE},
            System::Threading::{
                GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            },
        };
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if !handle.is_null() {
            let mut code = 0;
            GetExitCodeProcess(handle, &mut code);
            CloseHandle(handle);
            assert_ne!(
                code, STILL_ACTIVE as u32,
                "descendant survived cancellation"
            );
        }
    }
    #[cfg(unix)]
    unsafe {
        assert_eq!(
            libc::kill(pid as i32, 0),
            -1,
            "descendant survived cancellation"
        );
    }
    let (status, _) = h
        .call(
            "POST",
            "/sessions",
            json!({"requestId":"restart", "mode":"offline", "captureId":"synthetic"}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    h.manager.shutdown().await;
}

#[tokio::test]
async fn timeout_unknown_worker_error_and_bounded_stdout() {
    let h = Harness::new(Duration::from_millis(700)).await;
    let mut envelope = h.start().await;
    let id = envelope["id"].as_str().unwrap().to_string();
    let (status, _) = h
        .cmd(&envelope, "error", "navigate", json!({"action":"error"}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    envelope = h.idle(&id).await;
    assert_eq!(envelope["error"]["code"], "operation_failed");
    assert!(!envelope["error"]["message"]
        .as_str()
        .unwrap()
        .contains("secret.example"));
    assert_eq!(envelope["state"], "paused");
    let (status, _) = h
        .cmd(&envelope, "timeout", "mutate", json!({"action":"hang"}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    envelope = h.idle(&id).await;
    assert_eq!(envelope["state"], "failed");
    assert_eq!(envelope["error"]["code"], "timeout_unknown");
    let (status, _) = h.cmd(&envelope, "retry", "snapshot", json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    h.call("DELETE", &format!("/sessions/{id}"), json!({}))
        .await;
    h.manager.shutdown().await;
    // Oversized-output framing must not race the intentionally short timeout above.
    let h = Harness::new(Duration::from_secs(10)).await;
    let (status, e) = h
        .call(
            "POST",
            "/sessions",
            json!({"requestId":"oversize-start", "mode":"offline", "captureId":"synthetic"}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    envelope = h.idle(e["id"].as_str().unwrap()).await;
    let (status, _) = h
        .cmd(
            &envelope,
            "oversized",
            "navigate",
            json!({"action":"oversized"}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let envelope = h.idle(e["id"].as_str().unwrap()).await;
    assert_eq!(envelope["state"], "failed");
    assert_eq!(envelope["error"]["code"], "worker_failed");
    h.manager.shutdown().await;
}

#[tokio::test]
async fn real_native_offline_gateway_roundtrip() {
    let workspace = tempfile::tempdir().unwrap();
    let config = Config::new(workspace.path().to_path_buf(), None, None, None);
    let manager = Manager::new(config).await;
    assert!(
        manager.availability.is_none(),
        "Local native debugger unavailable: {:?}",
        manager.availability
    );
    let app = Router::new().nest("/api/debugger", router(manager.clone()));
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/debugger/capabilities")
                .header(header::HOST, "localhost:3000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let cookie = response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let csrf = decode(response).await["csrfToken"]
        .as_str()
        .unwrap()
        .to_string();
    let h = Harness {
        manager,
        app,
        cookie,
        csrf,
        _dir: workspace,
        script: PathBuf::new(),
    };
    let (status, envelope) = h.call("POST", "/sessions", json!({"requestId":"real-start", "mode":"offline", "captureId":"source-demo", "buildInfoId":"source-demo-build"})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{envelope}");
    let id = envelope["id"].as_str().unwrap();
    let mut current = h.idle(id).await;
    assert!(current["error"].is_null(), "{current}");
    assert_eq!(current["state"], "paused");
    assert_eq!(current["snapshot"]["capabilities"]["source"], true);
    let mut native=crate::desktop::debug_session::DebugSession::default();
    let launch=native.begin_start().unwrap();
    native.accept(&launch,&envelope).unwrap();
    native.accept(&launch,&current).unwrap();
    assert!(!native.snapshot.as_ref().unwrap().panes.source.is_null());
    assert!(!native.snapshot.as_ref().unwrap().panes.disassembly.is_null());
    for (request,method,params) in [
        ("native-window","snapshot",json!({"memoryOffset":"0","storageSlots":["0"],"sourceStart":"1","aroundPc":"0"})),
        ("native-opcode","navigate",json!({"action":"step_opcode"})),
        ("native-line","navigate",json!({"action":"step_line"})),
        ("native-next","navigate",json!({"action":"next_line"})),
        ("native-restart","navigate",json!({"action":"restart"})),
        ("native-discard-mutation","evaluate",json!({"expression":"feeBps = 999","keep":false})),
        ("native-discard-check","evaluate",json!({"expression":"feeBps","keep":false})),
    ] {
        let ticket=native.begin_command(method,params.get("action").and_then(Value::as_str)).unwrap();
        let (status,accepted)=h.cmd(&current,request,method,params).await;
        assert_eq!(status,StatusCode::ACCEPTED,"{request}: {accepted}");
        native.accept(&ticket,&accepted).unwrap();
        current=h.idle(id).await;
        assert!(current["error"].is_null(),"{request}: {current}");
        native.accept(&ticket,&current).unwrap();
        if request=="native-discard-check" {
            assert!(!current["result"].to_string().contains("999"),"discarded mutation persisted: {current}");
        }
    }
    for (request, method, params) in [
        (
            "real-break",
            "breakpoint",
            json!({"action":"add", "location":"withdraw"}),
        ),
        ("real-continue", "navigate", json!({"action":"continue"})),
        (
            "real-evaluate",
            "evaluate",
            json!({"expression":"feeBps", "keep":false}),
        ),
        (
            "real-mutate",
            "mutate",
            json!({"target":"storage", "key":"0", "value":"1000"}),
        ),
        ("real-export", "export_capture", json!({})),
    ] {
        let (status, accepted) = h.cmd(&current, request, method, params).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
        current = h.idle(id).await;
        assert!(current["error"].is_null(), "{request}: {current}");
        assert!(!current["snapshot"].is_null());
    }
    let (_, cancelled) = h
        .call("DELETE", &format!("/sessions/{id}"), json!({}))
        .await;
    assert_eq!(cancelled["state"], "cancelled");
    h.manager.shutdown().await;
}

#[tokio::test]
async fn websocket_ownership_origin_reconnect_and_resync() {
    use futures::StreamExt;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let h = Harness::new(Duration::from_secs(5)).await;
    let initial = h.start().await;
    let id = initial["id"].as_str().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = h.app.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let url = format!("ws://{address}/api/debugger/sessions/{id}/events?after=0");
    let mut request = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert(header::HOST, "localhost:3000".parse().unwrap());
    request
        .headers_mut()
        .insert(header::ORIGIN, "http://evil.test".parse().unwrap());
    request
        .headers_mut()
        .insert(header::COOKIE, h.cookie.parse().unwrap());
    assert!(tokio_tungstenite::connect_async(request.clone())
        .await
        .is_err());
    request
        .headers_mut()
        .insert(header::ORIGIN, "http://localhost:3000".parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(request.clone())
        .await
        .unwrap();
    let mut last = 0;
    for _ in 0..2 {
        let event = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let envelope: Value = serde_json::from_str(event.to_text().unwrap()).unwrap();
        assert!(envelope["sequence"].as_u64().unwrap() > last);
        last = envelope["sequence"].as_u64().unwrap();
    }
    socket.close(None).await.unwrap();
    let (_, accepted) = h
        .cmd(&initial, "ws-step", "navigate", json!({"action":"finish"}))
        .await;
    assert_eq!(accepted["busy"], true);
    let finished = h.idle(id).await;
    assert_eq!(finished["state"], "finished");
    let url = format!("ws://{address}/api/debugger/sessions/{id}/events?after=99999");
    let mut request = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert(header::HOST, "localhost:3000".parse().unwrap());
    request
        .headers_mut()
        .insert(header::ORIGIN, "http://localhost:3000".parse().unwrap());
    request
        .headers_mut()
        .insert(header::COOKIE, h.cookie.parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let event = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let envelope: Value = serde_json::from_str(event.to_text().unwrap()).unwrap();
    assert_eq!(envelope["sequence"], finished["sequence"]);
    assert_eq!(envelope["state"], "finished");
    socket.close(None).await.unwrap();
    server.abort();
    h.manager.shutdown().await;
}

#[tokio::test]
async fn unavailable_engine_does_not_crash_application() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::new(
        dir.path().to_path_buf(),
        Some(dir.path().join("nonexistent-python")),
        None,
        None,
    );
    let manager = Manager::new(config).await;
    assert!(manager.availability.is_some());
    let app = Router::new().nest("/api/debugger", router(manager));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/debugger/capabilities")
                .header(header::HOST, "localhost:3000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = decode(response).await;
    assert_eq!(body["available"], false);
    assert!(body["reason"]
        .as_str()
        .unwrap()
        .contains("--debugger-python"));
}
