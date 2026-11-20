use serde_json::{json, Value};
use std::time::Duration;

const BASE: &str = "http://127.0.0.1:3000";
const HOST: &str = "127.0.0.1:3000";
const MAX_BODY: usize = 8 * 1024 * 1024;
const MAX_RESOURCE_REPLY: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DebugLaunchRequest {
    Synthetic,
    Foundry {
        target: String,
        r#match: Option<String>,
        trusted: bool,
        /// Run from the test's first line to the first breakpoint instead.
        run_to_breakpoint: bool,
        breakpoints: Vec<String>,
    },
    Offline {
        capture_id: String,
        build_info_id: Option<String>,
    },
}

// The bytes and request ID stay together, including across an ambiguous launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedLaunch {
    request_id: String,
    body: Vec<u8>,
}

impl DebugLaunchRequest {
    pub fn prepare(&self) -> Result<PreparedLaunch, String> {
        let id = request_id();
        let mut body = json!({ "requestId": id });
        match self {
            Self::Synthetic => {
                body["mode"] = json!("offline");
                body["captureId"] = json!("source-demo");
                body["buildInfoId"] = json!("source-demo-build");
            }
            Self::Foundry {
                target,
                r#match,
                trusted,
                run_to_breakpoint,
                breakpoints,
            } => {
                if !trusted {
                    return Err("Confirm that this workspace and Foundry target are trusted before running local code.".into());
                }
                if !valid_target(target) {
                    return Err(
                        "Foundry target must be a workspace-relative .sol file without traversal."
                            .into(),
                    );
                }
                body["mode"] = json!("foundry");
                body["target"] = json!(target);
                if let Some(pattern) = r#match {
                    if pattern.is_empty()
                        || pattern.chars().count() > 4096
                        || pattern.chars().any(char::is_control)
                    {
                        return Err(
                            "Foundry test match must contain 1–4096 non-control characters.".into(),
                        );
                    }
                    body["match"] = json!(pattern);
                }
                if breakpoints.len() > 32
                    || breakpoints.iter().any(|spec| spec.trim().is_empty() || spec.len() > 512 || spec.chars().any(char::is_control))
                {
                    return Err("Breakpoints: at most 32 of File.sol:LINE, *PC or a function name.".into());
                }
                if !breakpoints.is_empty() {
                    body["breakpoints"] = json!(breakpoints);
                }
                if *run_to_breakpoint {
                    body["pauseAtEntry"] = json!(false);
                }
            }
            Self::Offline {
                capture_id,
                build_info_id,
            } => {
                if !valid_id(capture_id) {
                    return Err("Capture resource ID must contain 1–128 ASCII letters, digits, underscores, or hyphens.".into());
                }
                body["mode"] = json!("offline");
                body["captureId"] = json!(capture_id);
                if let Some(id) = build_info_id {
                    if !valid_id(id) {
                        return Err("Build info resource ID must contain 1–128 ASCII letters, digits, underscores, or hyphens.".into());
                    }
                    body["buildInfoId"] = json!(id);
                }
            }
        }
        let body = serde_json::to_vec(&body)
            .map_err(|_| "Could not prepare debugger launch.".to_string())?;
        Ok(PreparedLaunch {
            request_id: id,
            body,
        })
    }
}

impl PreparedLaunch {
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn valid_target(target: &str) -> bool {
    !target.is_empty()
        && target.chars().count() <= 4096
        && target.ends_with(".sol")
        && !target.contains(['\\', ':'])
        && !target.chars().any(char::is_control)
        && target
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

#[derive(Debug)]
pub struct StartFailure {
    pub message: String,
    pub request: PreparedLaunch,
    pub ambiguous: bool,
}

/// Launches with the stored owner token, renewing it once when the gateway
/// reports that this browser owner expired. `csrf` holds the token to keep.
pub async fn start(
    http: &reqwest::Client,
    csrf: &mut String,
    request: &PreparedLaunch,
) -> Result<Value, StartFailure> {
    let failure = |(message, ambiguous)| StartFailure {
        message,
        request: request.clone(),
        ambiguous,
    };
    if !csrf.is_empty() {
        match launch_at(http, csrf, request, BASE).await {
            Launch::Done(result) => return result.map_err(failure),
            Launch::OwnerExpired => {}
        }
    }
    *csrf = super::session::establish_debugger(http)
        .await
        .map_err(|message| failure((message, false)))?;
    match launch_at(http, csrf, request, BASE).await {
        Launch::Done(result) => result.map_err(failure),
        Launch::OwnerExpired => Err(failure((OWNER_LOST.into(), false))),
    }
}

const OWNER_LOST: &str = "The debugger gateway did not keep the renewed owner; no worker was accepted.";

enum Launch {
    Done(Result<Value, (String, bool)>),
    /// The owner idled out before this request; it had no live worker, so
    /// nothing was accepted and a renewed owner may safely retry.
    OwnerExpired,
}

pub async fn reconcile_cancel(
    http: &reqwest::Client,
    csrf: &str,
    request: &PreparedLaunch,
) -> Result<(), String> {
    reconcile_at(http, csrf, request, BASE).await
}

async fn reconcile_at(
    http: &reqwest::Client,
    csrf: &str,
    request: &PreparedLaunch,
    base: &str,
) -> Result<(), String> {
    // An expired owner cannot own a live worker, so the original launch is
    // either absent or already disposed.
    let value = match launch_at(http, csrf, request, base).await {
        Launch::OwnerExpired => return Ok(()),
        Launch::Done(result) => result
            .map_err(|(message, _)| format!("Could not reconcile the original launch: {message}"))?,
    };
    let session = value["id"]
        .as_str()
        .ok_or("Cannot identify the launch to dispose it.")?;
    let url = format!("{base}/api/debugger/sessions/{session}");
    let response = http
        .delete(&url)
        .header("host", HOST)
        .header("x-chasm-csrf", csrf)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|_| "Could not confirm disposal of the reconciled worker.".to_string())?;
    if response.url().as_str() != url {
        return Err("Reconciled worker disposal was redirected; retry Cancel.".into());
    }
    if response.status().is_success() {
        Ok(())
    } else {
        Err("Reconciled worker disposal was rejected; retry Cancel.".into())
    }
}

#[cfg(test)]
async fn send_start_at(
    http: &reqwest::Client,
    csrf: &str,
    request: &PreparedLaunch,
    base: &str,
) -> Result<Value, (String, bool)> {
    match launch_at(http, csrf, request, base).await {
        Launch::Done(result) => result,
        Launch::OwnerExpired => Err((OWNER_LOST.into(), false)),
    }
}

async fn launch_at(
    http: &reqwest::Client,
    csrf: &str,
    request: &PreparedLaunch,
    base: &str,
) -> Launch {
    let url = format!("{base}/api/debugger/sessions");
    let response = match http
        .post(&url)
        .header("host", HOST)
        .header("x-chasm-csrf", csrf)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .timeout(Duration::from_secs(10))
        .body(request.body.clone())
        .send()
        .await
    {
        Ok(response) => response,
        Err(_) => {
            return Launch::Done(Err((
                "Launch acknowledgement was lost or timed out; its outcome is not known.".into(),
                true,
            )))
        }
    };
    if response.url().as_str() != url {
        return Launch::Done(Err((
            "Launch acknowledgement was redirected; reconcile the original request.".into(),
            true,
        )));
    }
    if response.status() == reqwest::StatusCode::FORBIDDEN {
        let bytes = read_bounded(response, MAX_RESOURCE_REPLY).await.unwrap_or_default();
        if owner_expired(&bytes) {
            return Launch::OwnerExpired;
        }
        return Launch::Done(Err((
            format!("The gateway rejected this launch ({}).", reqwest::StatusCode::FORBIDDEN),
            false,
        )));
    }
    Launch::Done(read_launch(response).await)
}

pub(super) fn owner_expired(body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(body).is_ok_and(|value| {
        value["error"]["code"].as_str() == Some(crate::debugger::OWNER_EXPIRED)
    })
}

async fn read_launch(response: reqwest::Response) -> Result<Value, (String, bool)> {
    let status = response.status();
    if !status.is_success() {
        // A conflicting or failed server response may follow an earlier accepted POST.
        let ambiguous = status.is_server_error()
            || status == reqwest::StatusCode::CONFLICT
            || status == reqwest::StatusCode::REQUEST_TIMEOUT
            || status.is_redirection();
        return Err((
            if ambiguous {
                format!("The gateway could not confirm the launch outcome ({status}); reconcile before restarting.")
            } else {
                format!("The gateway rejected this launch ({status}).")
            },
            ambiguous,
        ));
    }
    let bytes = read_bounded(response, MAX_BODY)
        .await
        .map_err(|message| (message, true))?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
        (
            "Launch acknowledgement is malformed; reconcile before restarting.".into(),
            true,
        )
    })?;
    validate_ack(value)
}

fn validate_ack(value: Value) -> Result<Value, (String, bool)> {
    let valid = serde_json::from_value::<super::debug_session::Envelope>(value.clone())
        .ok()
        .is_some_and(|envelope| {
            valid_id(&envelope.id)
                && envelope.generation > 0
                && envelope.state != super::debug_session::Phase::Idle
        });
    if valid {
        Ok(value)
    } else {
        Err(("Launch acknowledgement has no usable session identity. Use Cancel to reconcile the original request.".into(), true))
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceKind {
    Capture,
    BuildInfo,
}

#[cfg(test)]
impl ResourceKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Capture => "capture",
            Self::BuildInfo => "build_info",
        }
    }
}

#[cfg(test)]
#[derive(Clone, Debug, serde::Deserialize, PartialEq, Eq)]
pub struct ResourceInfo {
    pub id: String,
    pub name: String,
    pub kind: String,
}

#[cfg(test)]
async fn upload_at(
    http: &reqwest::Client,
    csrf: &str,
    kind: ResourceKind,
    name: &str,
    content: &Value,
    base: &str,
) -> Result<ResourceInfo, String> {
    if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        return Err("Resource name must contain 1–128 non-control bytes.".into());
    }
    if !content.is_object() {
        return Err("Debugger resource must be a JSON object.".into());
    }
    #[derive(serde::Serialize)]
    struct Upload<'a> {
        kind: &'static str,
        name: &'a str,
        content: &'a Value,
    }
    let mut body = Vec::new();
    let mut writer = BoundedWriter {
        bytes: &mut body,
        limit: MAX_BODY,
        exceeded: false,
    };
    let result = serde_json::to_writer(
        &mut writer,
        &Upload {
            kind: kind.as_str(),
            name,
            content,
        },
    );
    if writer.exceeded {
        return Err("Debugger resource upload exceeds the 8 MiB request limit.".into());
    }
    result.map_err(|_| "Could not prepare debugger resource.".to_string())?;
    let url = format!("{base}/api/debugger/resources");
    let response = http
        .post(&url)
        .header("host", HOST)
        .header("x-chasm-csrf", csrf)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .timeout(Duration::from_secs(10))
        .body(body)
        .send()
        .await
        .map_err(|_| "Debugger resource upload failed or timed out.".to_string())?;
    if response.url().as_str() != url {
        return Err("Debugger resource upload was redirected.".into());
    }
    if !response.status().is_success() {
        return Err(format!(
            "The gateway rejected this resource upload ({}).",
            response.status()
        ));
    }
    let bytes = read_bounded(response, MAX_RESOURCE_REPLY).await?;
    let resource: ResourceInfo = serde_json::from_slice(&bytes)
        .map_err(|_| "Debugger resource acknowledgement is malformed.".to_string())?;
    if !valid_id(&resource.id) || resource.kind != kind.as_str() || resource.name != name {
        return Err(
            "Debugger resource acknowledgement does not identify the uploaded resource.".into(),
        );
    }
    Ok(resource)
}

#[cfg(test)]
struct BoundedWriter<'a> {
    bytes: &'a mut Vec<u8>,
    limit: usize,
    exceeded: bool,
}

#[cfg(test)]
impl std::io::Write for BoundedWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "resource exceeds request limit",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

async fn read_bounded(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Could not finish reading the debugger acknowledgement.".to_string())?
    {
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err("Debugger acknowledgement exceeds its limit.".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Bytes, extract::State, http::StatusCode, routing::post, Json, Router};
    use std::sync::{Arc, Mutex};

    fn acknowledgement(id: &str) -> Value {
        json!({"id":id,"generation":1,"revision":0,"sequence":1,"state":"preparing","busy":true,"snapshot":null,"error":null,"result":null})
    }

    fn launch_body(request: &PreparedLaunch) -> Value {
        serde_json::from_slice(&request.body).unwrap()
    }

    fn expected(id: &str, request: Value) -> Value {
        let mut request = request;
        request["requestId"] = json!(id);
        request
    }

    #[test]
    fn launch_bodies_match_gateway_schema_for_each_mode() {
        for (mode, fields) in [
            (
                DebugLaunchRequest::Synthetic,
                json!({"mode":"offline", "captureId":"source-demo", "buildInfoId":"source-demo-build"}),
            ),
            (
                DebugLaunchRequest::Foundry {
                    target: "test/Counter.t.sol".into(),
                    r#match: Some("testCounter".into()),
                    trusted: true,
                run_to_breakpoint: false,
                breakpoints: Vec::new(),
                },
                json!({"mode":"foundry", "target":"test/Counter.t.sol", "match":"testCounter"}),
            ),
            (
                DebugLaunchRequest::Foundry {
                    target: "test/Counter.t.sol".into(),
                    r#match: None,
                    trusted: true,
                run_to_breakpoint: false,
                breakpoints: Vec::new(),
                },
                json!({"mode":"foundry", "target":"test/Counter.t.sol"}),
            ),
            (
                DebugLaunchRequest::Offline {
                    capture_id: "capture-1".into(),
                    build_info_id: Some("build_1".into()),
                },
                json!({"mode":"offline", "captureId":"capture-1", "buildInfoId":"build_1"}),
            ),
            (
                DebugLaunchRequest::Offline {
                    capture_id: "capture-1".into(),
                    build_info_id: None,
                },
                json!({"mode":"offline", "captureId":"capture-1"}),
            ),
        ] {
            let request = mode.prepare().unwrap();
            assert_eq!(
                launch_body(&request),
                expected(request.request_id(), fields)
            );
            assert!(request.request_id().starts_with("desktop-start-"));
        }
        assert_ne!(
            DebugLaunchRequest::Synthetic
                .prepare()
                .unwrap()
                .request_id(),
            DebugLaunchRequest::Synthetic
                .prepare()
                .unwrap()
                .request_id()
        );
    }

    #[test]
    fn invalid_launch_fields_fail_before_preparing() {
        for target in [
            "",
            "../outside.sol",
            "test/../outside.sol",
            "/etc/outside.sol",
            "C:/outside.sol",
            "folder\\outside.sol",
            "test/./file.sol",
            "test//file.sol",
            "file.txt",
            "file.sol\0",
        ] {
            let request = DebugLaunchRequest::Foundry {
                target: target.into(),
                r#match: None,
                trusted: true,
                run_to_breakpoint: false,
                breakpoints: Vec::new(),
            };
            assert!(request.prepare().is_err(), "accepted target {target:?}");
        }
        assert!(DebugLaunchRequest::Foundry {
            target: "test/file.sol".into(),
            r#match: None,
            trusted: false,
            run_to_breakpoint: false,
            breakpoints: Vec::new()
        }
        .prepare()
        .is_err());
        for pattern in ["", "test\nInjected"] {
            assert!(DebugLaunchRequest::Foundry {
                target: "test/file.sol".into(),
                r#match: Some(pattern.into()),
                trusted: true,
                run_to_breakpoint: false,
                breakpoints: Vec::new()
            }
            .prepare()
            .is_err());
        }
        assert!(DebugLaunchRequest::Foundry {
            target: "test/file.sol".into(),
            r#match: Some("a".repeat(4097)),
            trusted: true,
            run_to_breakpoint: false,
            breakpoints: Vec::new()
        }
        .prepare()
        .is_err());
        // Launch breakpoints and the entry pause reach the gateway body.
        let launch = DebugLaunchRequest::Foundry {
            target: "test/file.sol".into(),
            r#match: None,
            trusted: true,
            run_to_breakpoint: true,
            breakpoints: vec!["Vault.sol:12".into(), "*0x1a".into()],
        }
        .prepare()
        .unwrap();
        let body: Value = serde_json::from_slice(&launch.body).unwrap();
        assert_eq!((body["breakpoints"].clone(), body["pauseAtEntry"].clone()), (json!(["Vault.sol:12", "*0x1a"]), json!(false)));
        for breakpoints in [vec!["".to_string()], vec!["a
b".to_string()], vec!["x".to_string(); 33]] {
            assert!(DebugLaunchRequest::Foundry { target: "test/file.sol".into(), r#match: None, trusted: true, run_to_breakpoint: false, breakpoints }.prepare().is_err());
        }
        for id in [
            "".to_string(),
            "../capture".into(),
            "C:/capture".into(),
            "a/b".into(),
            "abc?x=1".into(),
            "unicode-λ".into(),
            "a".repeat(129),
        ] {
            assert!(
                DebugLaunchRequest::Offline {
                    capture_id: id.clone(),
                    build_info_id: None
                }
                .prepare()
                .is_err(),
                "accepted capture {id:?}"
            );
            assert!(
                DebugLaunchRequest::Offline {
                    capture_id: "capture".into(),
                    build_info_id: Some(id.clone())
                }
                .prepare()
                .is_err(),
                "accepted build {id:?}"
            );
        }
        assert!(DebugLaunchRequest::Offline {
            capture_id: "a".repeat(128),
            build_info_id: None
        }
        .prepare()
        .is_ok());
    }

    #[test]
    fn invalid_success_acknowledgements_keep_ambiguous_recovery() {
        for value in [
            json!({}),
            Value::Null,
            json!({"id":""}),
            json!({"id":"known","state":"unknown"}),
            acknowledgement("../worker"),
            acknowledgement("worker?other"),
            json!({"id":"worker", "generation":0, "revision":0, "sequence":1, "state":"preparing", "busy":true, "snapshot":null, "error":null, "result":null}),
        ] {
            let (message, ambiguous) = validate_ack(value).unwrap_err();
            assert!(ambiguous);
            assert!(message.contains("reconcile"));
            let mut session = super::super::debug_session::DebugSession::default();
            session.begin_start();
            session.launch_failed("same-request".into(), message, ambiguous);
            assert!(session.can_cancel());
            assert!(!session.can_start());
        }
        assert!(validate_ack(acknowledgement("known-worker")).is_ok());
    }

    #[derive(Default)]
    struct Fixture {
        requests: Vec<Vec<u8>>,
        accepted: Option<Vec<u8>>,
        creates: usize,
        deletes: usize,
        lost_ack: bool,
        malformed_ack: bool,
        response_status: Option<StatusCode>,
        response_body: Option<String>,
    }

    async fn launch(
        State(state): State<Arc<Mutex<Fixture>>>,
        bytes: Bytes,
    ) -> (StatusCode, String) {
        let mut state = state.lock().unwrap();
        state.requests.push(bytes.to_vec());
        if let Some(status) = state.response_status {
            let body = state.response_body.clone();
            return (status, body.unwrap_or_else(|| "fixture rejection".into()));
        }
        if let Some(original) = &state.accepted {
            if original != &bytes.to_vec() {
                return (StatusCode::CONFLICT, "different request".into());
            }
        } else {
            state.accepted = Some(bytes.to_vec());
            state.creates += 1;
        }
        if state.lost_ack {
            state.lost_ack = false;
            return (StatusCode::ACCEPTED, "{".into());
        }
        if state.malformed_ack {
            return (
                StatusCode::ACCEPTED,
                acknowledgement("../worker").to_string(),
            );
        }
        (
            StatusCode::ACCEPTED,
            acknowledgement("fixture-worker").to_string(),
        )
    }

    async fn dispose(State(state): State<Arc<Mutex<Fixture>>>) -> Json<Value> {
        state.lock().unwrap().deletes += 1;
        Json(json!({"state":"cancelled"}))
    }

    async fn fixture(state: Arc<Mutex<Fixture>>) -> (String, tokio::task::JoinHandle<()>) {
        let app = Router::new()
            .route("/api/debugger/sessions", post(launch))
            .route("/api/debugger/sessions/:id", axum::routing::delete(dispose))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (base, server)
    }

    #[tokio::test]
    async fn reconciliation_reuses_exact_request_and_never_creates_a_second_worker() {
        let requests = [
            DebugLaunchRequest::Synthetic,
            DebugLaunchRequest::Foundry {
                target: "test/Counter.t.sol".into(),
                r#match: Some("testCounter".into()),
                trusted: true,
                run_to_breakpoint: false,
                breakpoints: Vec::new(),
            },
            DebugLaunchRequest::Offline {
                capture_id: "resource_1".into(),
                build_info_id: Some("resource_2".into()),
            },
        ];
        for mode in requests {
            for already_accepted in [false, true] {
                let prepared = mode.prepare().unwrap();
                let state = Arc::new(Mutex::new(Fixture {
                    lost_ack: already_accepted,
                    ..Fixture::default()
                }));
                let (base, server) = fixture(state.clone()).await;
                let http = reqwest::Client::new();
                if already_accepted {
                    let (_, ambiguous) = send_start_at(&http, "synthetic-csrf", &prepared, &base)
                        .await
                        .unwrap_err();
                    assert!(ambiguous);
                }
                reconcile_at(&http, "synthetic-csrf", &prepared, &base)
                    .await
                    .unwrap();
                let state = state.lock().unwrap();
                assert_eq!(state.creates, 1);
                assert_eq!(state.deletes, 1);
                assert_eq!(state.requests.len(), if already_accepted { 2 } else { 1 });
                assert!(state.requests.iter().all(|body| body == &prepared.body));
                server.abort();
            }
        }
    }

    #[tokio::test]
    async fn server_errors_and_conflicts_leave_launch_outcome_ambiguous() {
        for (status, expected_ambiguous) in [
            (StatusCode::BAD_REQUEST, false),
            (StatusCode::FORBIDDEN, false),
            (StatusCode::CONFLICT, true),
            (StatusCode::REQUEST_TIMEOUT, true),
            (StatusCode::INTERNAL_SERVER_ERROR, true),
            (StatusCode::SERVICE_UNAVAILABLE, true),
        ] {
            let state = Arc::new(Mutex::new(Fixture {
                response_status: Some(status),
                ..Fixture::default()
            }));
            let (base, server) = fixture(state).await;
            let prepared = DebugLaunchRequest::Synthetic.prepare().unwrap();
            let (_, ambiguous) =
                send_start_at(&reqwest::Client::new(), "synthetic-csrf", &prepared, &base)
                    .await
                    .unwrap_err();
            assert_eq!(ambiguous, expected_ambiguous, "status {status}");
            server.abort();
        }
    }

    #[tokio::test]
    async fn only_an_expired_owner_rejection_allows_renewal() {
        let expired = json!({"error": {"code": crate::debugger::OWNER_EXPIRED, "message": "x"}});
        for (body, renew) in [
            (expired.to_string(), true),
            (json!({"error": {"code": "forbidden", "message": "x"}}).to_string(), false),
            ("fixture rejection".to_string(), false),
        ] {
            let state = Arc::new(Mutex::new(Fixture {
                response_status: Some(StatusCode::FORBIDDEN),
                response_body: Some(body.clone()),
                ..Fixture::default()
            }));
            let (base, server) = fixture(state.clone()).await;
            let prepared = DebugLaunchRequest::Synthetic.prepare().unwrap();
            let http = reqwest::Client::new();
            match launch_at(&http, "stale-csrf", &prepared, &base).await {
                Launch::OwnerExpired => assert!(renew, "{body}"),
                Launch::Done(result) => {
                    assert!(!renew, "{body}");
                    assert!(!result.unwrap_err().1, "a plain 403 accepted nothing");
                }
            }
            // An expired owner holds no worker, so reconciliation is a no-op.
            if renew {
                reconcile_at(&http, "stale-csrf", &prepared, &base).await.unwrap();
                assert_eq!(state.lock().unwrap().deletes, 0);
            }
            server.abort();
        }
    }

    #[tokio::test]
    async fn reconciliation_does_not_delete_when_ack_is_malformed() {
        let state = Arc::new(Mutex::new(Fixture {
            malformed_ack: true,
            ..Fixture::default()
        }));
        let (base, server) = fixture(state.clone()).await;
        let prepared = DebugLaunchRequest::Synthetic.prepare().unwrap();
        assert!(
            reconcile_at(&reqwest::Client::new(), "synthetic-csrf", &prepared, &base)
                .await
                .is_err()
        );
        assert_eq!(state.lock().unwrap().deletes, 0);
        server.abort();
    }

    #[derive(Default)]
    struct UploadFixture {
        bodies: Vec<Value>,
        reply: Option<Value>,
    }

    async fn upload(
        State(state): State<Arc<Mutex<UploadFixture>>>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let mut state = state.lock().unwrap();
        state.bodies.push(body.clone());
        Json(state.reply.clone().unwrap_or_else(
            || json!({"id":"resource_1", "name":body["name"], "kind":body["kind"]}),
        ))
    }

    #[tokio::test]
    async fn resource_upload_is_bounded_and_uses_gateway_schema() {
        let state = Arc::new(Mutex::new(UploadFixture::default()));
        let app = Router::new()
            .route("/api/debugger/resources", post(upload))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let http = reqwest::Client::new();
        for kind in [ResourceKind::Capture, ResourceKind::BuildInfo] {
            let content = json!({"steps":[]});
            let resource = upload_at(
                &http,
                "synthetic-csrf",
                kind,
                "selected.json",
                &content,
                &base,
            )
            .await
            .unwrap();
            assert_eq!(resource.id, "resource_1");
            assert_eq!(resource.kind, kind.as_str());
            assert_eq!(
                state.lock().unwrap().bodies.last().unwrap(),
                &json!({"kind":kind.as_str(), "name":"selected.json", "content":content})
            );
        }
        for (name, content) in [
            ("", json!({})),
            ("bad\nname", json!({})),
            ("name", json!([])),
            ("name", json!({"data":"x".repeat(MAX_BODY)})),
        ] {
            assert!(upload_at(
                &http,
                "synthetic-csrf",
                ResourceKind::Capture,
                name,
                &content,
                &base
            )
            .await
            .is_err());
        }
        assert_eq!(state.lock().unwrap().bodies.len(), 2);
        for reply in [
            json!({}),
            json!({"id":"../resource", "name":"selected.json", "kind":"capture"}),
            json!({"id":"resource", "name":"selected.json", "kind":"build_info"}),
        ] {
            state.lock().unwrap().reply = Some(reply);
            assert!(upload_at(
                &http,
                "synthetic-csrf",
                ResourceKind::Capture,
                "selected.json",
                &json!({}),
                &base
            )
            .await
            .is_err());
        }
        server.abort();
    }
}

fn request_id() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    format!("desktop-start-{}", ethers::utils::hex::encode(bytes))
}
