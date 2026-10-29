//! Census observations are available before consent and before a projection
//! exists. Unknown estimates must survive HTTP as null, not zero or omission.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use rosetta_protocol::{DiscoveriesResponse, DiscoveryRow};
use rosetta_server::{AppState, ConsentGate, Hub, HubConfig, router};
use tower::ServiceExt;

#[tokio::test]
async fn session_estimates_are_public_before_consent_without_a_projection() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("not-created.sqlite");
    let gate = Arc::new(ConsentGate::new());
    let rows: Vec<_> = [
        ("claude-code", true, Some(12)),
        ("pi", false, Some(0)),
        ("hermes", true, None),
    ]
    .into_iter()
    .map(|(kind, installed, sessions)| DiscoveryRow {
        kind: kind.into(),
        data_root: format!("~/.{kind}"),
        installed,
        files: if installed { 20 } else { 0 },
        sessions,
        bytes: 0,
        newest_modified_ms: 0,
        enabled: false,
    })
    .collect();
    gate.publish(rows.clone());
    let _consent = gate.arm();
    let app = router(
        AppState::new(Arc::new(Hub::new(HubConfig::default(), 1)), db.clone()).with_consent(gate),
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/discoveries")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let wire: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(wire["discoveries"][0]["sessions"], 12);
    assert_eq!(wire["discoveries"][1]["sessions"], 0);
    assert_eq!(
        wire["discoveries"][2].get("sessions"),
        Some(&serde_json::Value::Null)
    );
    let response: DiscoveriesResponse = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(response.discoveries, rows);
    assert!(!db.exists());
}
