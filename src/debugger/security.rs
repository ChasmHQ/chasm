use super::{random_id, GatewayError, Manager};
use axum::{
    extract::{Request, State},
    http::{header, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::sync::Arc;

pub const COOKIE: &str = "chasm_debugger_owner";

#[derive(Clone)]
pub struct Owner(pub(crate) String);

pub(super) async fn protect(
    State(manager): State<Arc<Manager>>,
    mut request: Request,
    next: Next,
) -> Response {
    if manager.stopping.load(std::sync::atomic::Ordering::Acquire) {
        return GatewayError::unavailable().into_response();
    }
    let headers = request.headers();
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let hosts = [
        format!("localhost:{}", manager.config.port),
        format!("127.0.0.1:{}", manager.config.port),
    ];
    if headers.get_all(header::HOST).iter().count() != 1
        || !hosts.iter().any(|allowed| host == allowed)
    {
        return GatewayError::forbidden("Debugger requires the configured loopback Host.")
            .into_response();
    }
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    let dev = origin.is_some() && origin == manager.config.dev_origin.as_deref();
    if (headers.contains_key(header::ORIGIN) && origin.is_none())
        || headers.get_all(header::ORIGIN).iter().count() > 1
        || origin.is_some_and(|origin| origin != format!("http://{host}") && !dev)
    {
        return GatewayError::forbidden("Debugger Origin is not allowed.").into_response();
    }
    if headers
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|site| {
            !matches!(site, "same-origin" | "none") && !(dev && site == "same-site")
        })
    {
        return GatewayError::forbidden("Cross-site debugger requests are not allowed.")
            .into_response();
    }
    let websocket = headers
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
    if websocket && origin.is_none() {
        return GatewayError::forbidden("Debugger WebSocket requires an Origin.").into_response();
    }
    // The only credential is an HttpOnly cookie; query parameters never authenticate.
    if request.uri().query().is_some_and(|query| {
        query.split('&').any(|part| {
            let key = part.split('=').next().unwrap_or("").to_ascii_lowercase();
            !matches!(key.as_str(), "after" | "sequence")
        })
    }) {
        return GatewayError::bad("Unsupported debugger query parameter.").into_response();
    }
    let origin = origin.map(str::to_owned);
    if request.method() == Method::OPTIONS {
        if !dev {
            return GatewayError::forbidden("Debugger preflight is not allowed.").into_response();
        }
        return cors(StatusCode::NO_CONTENT.into_response(), origin.as_deref());
    }
    let bootstrap =
        request.method() == Method::GET && request.uri().path().ends_with("/capabilities");
    let cookie_values: Vec<_> = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|part| part.trim().split_once('='))
        .filter(|(name, _)| *name == COOKIE)
        .map(|(_, value)| value.to_string())
        .collect();
    let mut set_cookie = None;
    let identity = {
        let mut registry = manager.registry.lock().unwrap();
        let known = if cookie_values.len() == 1 {
            registry
                .owners
                .get(&cookie_values[0])
                .map(|csrf| (cookie_values[0].clone(), csrf.clone()))
        } else {
            None
        };
        match known {
            Some(identity) => identity,
            None if bootstrap && cookie_values.len() <= 1 => {
                if registry.owners.len() >= 32 {
                    return GatewayError::conflict(
                        "Too many debugger browser owners; restart the local server.",
                    )
                    .into_response();
                }
                let owner = random_id();
                let csrf = random_id();
                registry.owners.insert(owner.clone(), csrf.clone());
                set_cookie = Some(format!(
                    "{COOKIE}={owner}; Path=/api/debugger; HttpOnly; SameSite=Strict"
                ));
                (owner, csrf)
            }
            _ => return GatewayError::owner_expired().into_response(),
        }
    };
    manager
        .registry
        .lock()
        .unwrap()
        .owner_activity
        .insert(identity.0.clone(), std::time::Instant::now());
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        let csrf = headers.get_all("x-chasm-csrf");
        if csrf.iter().count() != 1
            || csrf.iter().next().and_then(|v| v.to_str().ok()) != Some(identity.1.as_str())
        {
            return GatewayError::forbidden("Debugger CSRF token is missing or invalid.")
                .into_response();
        }
    }
    request.extensions_mut().insert(Owner(identity.0));
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    if let Some(cookie) = set_cookie {
        response
            .headers_mut()
            .insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
    }
    cors(response, if dev { origin.as_deref() } else { None })
}

fn cors(mut response: Response, origin: Option<&str>) -> Response {
    if let Some(origin) = origin {
        let headers = response.headers_mut();
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_str(origin).unwrap(),
        );
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
            HeaderValue::from_static("true"),
        );
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, POST, DELETE, OPTIONS"),
        );
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("Content-Type, X-Chasm-CSRF"),
        );
        headers.insert(header::VARY, HeaderValue::from_static("Origin"));
    }
    response
}
