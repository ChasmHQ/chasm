//! Bounded retries for idempotent reads from public RPCs and Sourcify, which
//! answer HTTP 429 (or 503) when rate limited. Waits follow `Retry-After`
//! when given, else back off exponentially; the total wait is capped. Never
//! used for anything that changes state: a send is never repeated.
use std::time::Duration;

/// Attempts per read, including the first.
pub const ATTEMPTS: u32 = 5;
/// Longest single wait and longest total wait across one read.
const MAX_WAIT: Duration = Duration::from_secs(8);
const MAX_TOTAL: Duration = Duration::from_secs(20);
const FIRST_WAIT: Duration = Duration::from_millis(500);

/// Whether a status means "try again later".
pub fn retryable(status: u16) -> bool {
    status == 429 || status == 503
}

/// How long to wait before attempt `attempt` (1 = the first retry).
pub fn wait(attempt: u32, retry_after: Option<&str>) -> Duration {
    let suggested = retry_after.and_then(|value| value.trim().parse::<u64>().ok()).map(Duration::from_secs);
    suggested.unwrap_or_else(|| FIRST_WAIT * 2u32.saturating_pow(attempt.saturating_sub(1))).min(MAX_WAIT)
}

/// Sends `build()` until it is not rate limited, at most `ATTEMPTS` times and
/// `MAX_TOTAL` of waiting. The last response is returned either way, so the
/// caller still reports a final 429.
pub async fn send(build: impl Fn() -> reqwest::RequestBuilder) -> Result<reqwest::Response, reqwest::Error> {
    let mut waited = Duration::ZERO;
    let mut attempt = 0;
    loop {
        let response = build().send().await?;
        let status = response.status().as_u16();
        attempt += 1;
        if !retryable(status) || attempt >= ATTEMPTS {
            return Ok(response);
        }
        let pause = wait(attempt, response.headers().get(reqwest::header::RETRY_AFTER).and_then(|value| value.to_str().ok()));
        if waited + pause > MAX_TOTAL {
            return Ok(response);
        }
        waited += pause;
        tokio::time::sleep(pause).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn waits_follow_retry_after_then_back_off_with_a_cap() {
        assert!(retryable(429) && retryable(503) && !retryable(500) && !retryable(200));
        assert_eq!(wait(1, Some("2")), Duration::from_secs(2));
        assert_eq!(wait(1, Some("600")), MAX_WAIT, "a long Retry-After is capped");
        assert_eq!(wait(1, None), Duration::from_millis(500));
        assert_eq!(wait(3, None), Duration::from_secs(2));
        assert_eq!(wait(9, Some("soon")), MAX_WAIT);
    }

    /// A local server that answers 429 `failures` times, then 200.
    async fn server(failures: usize) -> (String, Arc<AtomicUsize>) {
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let app = axum::Router::new().route(
            "/",
            axum::routing::post(move || {
                let counter = counter.clone();
                async move {
                    let seen = counter.fetch_add(1, Ordering::SeqCst);
                    if seen < failures {
                        (axum::http::StatusCode::TOO_MANY_REQUESTS, [(axum::http::header::RETRY_AFTER, "0")], "slow down").into_response()
                    } else {
                        (axum::http::StatusCode::OK, "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":\"0x1\"}").into_response()
                    }
                }
            }),
        );
        use axum::response::IntoResponse;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, hits)
    }

    #[tokio::test]
    async fn a_rate_limited_read_succeeds_once_the_limit_lifts() {
        let (url, hits) = server(2).await;
        let client = reqwest::Client::new();
        let response = send(|| client.post(&url).body("{}")).await.unwrap();
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(hits.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn a_lasting_limit_stops_after_the_attempt_cap() {
        let (url, hits) = server(100).await;
        let client = reqwest::Client::new();
        let response = send(|| client.post(&url).body("{}")).await.unwrap();
        assert_eq!(response.status().as_u16(), 429, "the final 429 reaches the caller");
        assert_eq!(hits.load(Ordering::SeqCst), ATTEMPTS as usize);
    }
}
