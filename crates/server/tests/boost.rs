//! The boost route says `queued: true` only while a drainer is running. A
//! serve with no ledger attached (every mode but the disk cold engine) and a
//! ledger its engine has closed both answer `queued: false`, with the same
//! response shape; a request with no session id is a 400 in every mode.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use rosetta_server::{AppState, BoostLedger, Hub, HubConfig, router};
use tempfile::TempDir;

async fn boost(app: &Router) -> (StatusCode, Value) {
    boost_id(app, "s1").await
}

async fn boost_id(app: &Router, session_id: &str) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/ingest/boost")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "session_id": session_id }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn boost_is_queued_only_while_an_open_ledger_is_attached() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("projection.sqlite");
    let hub = Arc::new(Hub::new(HubConfig::default(), 1));

    let unattached = router(AppState::new(hub.clone(), db.clone()));
    assert_eq!(
        boost(&unattached).await,
        (StatusCode::OK, json!({"queued": false}))
    );

    assert_eq!(boost_id(&unattached, "").await.0, StatusCode::BAD_REQUEST);

    let ledger = Arc::new(BoostLedger::new());
    let draining = router(AppState::new(hub, db).with_boost(ledger.clone()));
    assert_eq!(
        boost(&draining).await,
        (StatusCode::OK, json!({"queued": true}))
    );
    assert_eq!(boost_id(&draining, "").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(ledger.drain(), vec!["s1".to_string()]);

    // The engine's loop ended: nothing will read the queue again.
    ledger.close();
    assert_eq!(
        boost(&draining).await,
        (StatusCode::OK, json!({"queued": false}))
    );
    assert!(ledger.drain().is_empty());
    assert_eq!(boost_id(&draining, "").await.0, StatusCode::BAD_REQUEST);
}
