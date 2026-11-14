use super::application::{self, AppError};
use super::chain_read::{BlockRef, ReadRequest};
use super::investigation::Investigation;
use axum::{extract::State, routing::post, Json, Router};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

struct Fixture {
    ready: AtomicBool,
    release: tokio::sync::Notify,
}

async fn rpc(State(fixture): State<Arc<Fixture>>, Json(request): Json<Value>) -> Json<Value> {
    let params = &request["params"];
    let result = match request["method"].as_str().unwrap_or_default() {
        "eth_chainId" => json!("0x7a69"),
        "eth_getBlockByNumber" if params[0] == "0x0" => {
            json!({"number":"0x0","hash":format!("0x{}","aa".repeat(32))})
        }
        "eth_getBlockByNumber" => {
            if params[1] == true {
                fixture.ready.store(true, Ordering::SeqCst);
                fixture.release.notified().await;
            }
            json!({"number":"0x10","hash":format!("0x{}","bb".repeat(32)),"transactions":[]})
        }
        _ => Value::Null,
    };
    Json(json!({"jsonrpc":"2.0","id":1,"result":result}))
}

#[tokio::test]
async fn source_switch_during_read_discards_capture_before_history_insertion() {
    let fixture = Arc::new(Fixture {
        ready: AtomicBool::new(false),
        release: tokio::sync::Notify::new(),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = Router::new()
        .route("/", post(rpc))
        .with_state(fixture.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let root = tempfile::tempdir().unwrap();
    let inv = Arc::new(Investigation::new(root.path().into()));
    let state = super::session::DesktopState::new(root.path().into(), 1.0);
    let context = application::context(&state);
    inv.bind_source(Some(context.clone()));
    let worker = inv.clone();
    let query = tokio::spawn(async move {
        worker
            .read(
                &url,
                context,
                ReadRequest::Block {
                    selector: BlockRef::Number(16),
                    full_tx: true,
                },
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !fixture.ready.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    inv.bind_source(None);
    fixture.release.notify_one();
    let error: AppError = tokio::time::timeout(std::time::Duration::from_secs(3), query)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, "stale_source");
    let history = inv
        .execute(None, super::investigation::AnalysisRequest::History)
        .await
        .unwrap();
    assert!(history["captures"].as_array().unwrap().is_empty());
    server.abort();
}
