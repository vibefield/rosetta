use super::*;

fn open(temp: &tempfile::TempDir) -> Arc<ColdControl> {
    ColdControl::open(
        &temp.path().join("index.db"),
        &temp.path().join("index.ledger"),
        false,
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap()
    .unwrap()
}
fn command(control: &ColdControl, action: IngestionAction, key: &str) -> IngestionReply {
    let run = control.snapshot();
    control
        .command(
            &run.id,
            &run.index_id,
            key,
            action,
            IngestionControlRequest {
                expected_revision: run.revision,
            },
        )
        .unwrap()
}

#[test]
fn accepted_intent_survives_restart_and_retries_precede_revision_checks() {
    let temp = tempfile::tempdir().unwrap();
    let control = open(&temp);
    let before = control.snapshot();
    let request = IngestionControlRequest {
        expected_revision: before.revision,
    };
    assert!(command(&control, IngestionAction::Pause, "key1").accepted);
    assert!(control.cancel.load(Ordering::SeqCst));
    assert_eq!(control.snapshot().state, IngestionState::Pausing);
    let retry = control
        .command(
            &before.id,
            &before.index_id,
            "key1",
            IngestionAction::Pause,
            request.clone(),
        )
        .unwrap();
    assert_eq!(retry.run.state, IngestionState::Pausing);
    assert!(!retry.accepted);
    drop(control);
    let control = open(&temp);
    assert_eq!(control.snapshot().id, before.id);
    assert_eq!(control.snapshot().state, IngestionState::Paused);
    assert_eq!(
        control
            .command(
                &before.id,
                &before.index_id,
                "key1",
                IngestionAction::Pause,
                request.clone()
            )
            .unwrap()
            .run
            .state,
        IngestionState::Paused
    );
    assert_eq!(
        control
            .command(
                &before.id,
                &before.index_id,
                "key2",
                IngestionAction::Resume,
                request.clone()
            )
            .unwrap_err()
            .code,
        ErrorCode::RevisionConflict
    );
    assert_eq!(
        control
            .command(
                &before.id,
                &before.index_id,
                "key1",
                IngestionAction::Resume,
                request
            )
            .unwrap_err()
            .code,
        ErrorCode::IdempotencyConflict
    );
    assert!(command(&control, IngestionAction::Resume, "key3").accepted);
    assert!(!control.cancel.load(Ordering::SeqCst));
    assert_eq!(
        command(&control, IngestionAction::Resume, "key4").run.state,
        IngestionState::Running
    );
}

#[test]
fn pause_and_publication_cannot_both_win_and_pausing_cannot_resume() {
    let temp = tempfile::tempdir().unwrap();
    let control = open(&temp);
    command(&control, IngestionAction::Pause, "pause");
    assert!(control.publishing().is_err());
    let run = control.snapshot();
    assert_eq!(
        control
            .command(
                &run.id,
                &run.index_id,
                "resume",
                IngestionAction::Resume,
                IngestionControlRequest {
                    expected_revision: run.revision
                }
            )
            .unwrap_err()
            .code,
        ErrorCode::InvalidTransition
    );
    control.stopped(None).unwrap();
    command(&control, IngestionAction::Resume, "resume");
    control.publishing().unwrap();
    let run = control.snapshot();
    assert!(run.allowed_actions.is_empty());
    assert_eq!(
        control
            .command(
                &run.id,
                &run.index_id,
                "pause2",
                IngestionAction::Pause,
                IngestionControlRequest {
                    expected_revision: run.revision
                }
            )
            .unwrap_err()
            .code,
        ErrorCode::PublicationInProgress
    );
    control.succeeded().unwrap();
    assert!(control.snapshot().allowed_actions.is_empty());
}

#[test]
fn index_binding_and_failed_state_survive_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let control = open(&temp);
    let run = control.snapshot();
    assert_eq!(
        control
            .command(
                &run.id,
                "another-index",
                "key",
                IngestionAction::Pause,
                IngestionControlRequest {
                    expected_revision: run.revision
                }
            )
            .unwrap_err()
            .code,
        ErrorCode::IndexChanged
    );
    control.stopped(Some("source unavailable")).unwrap();
    drop(control);
    let control = open(&temp);
    assert_eq!(control.snapshot().state, IngestionState::Failed);
    assert_eq!(
        control.snapshot().error.as_deref(),
        Some("source unavailable")
    );
    assert!(
        ColdControl::open(
            &temp.path().join("index.db"),
            &temp.path().join("different-ledger"),
            false,
            Arc::new(AtomicBool::new(false))
        )
        .is_err()
    );
    command(&control, IngestionAction::Resume, "resume");
    assert_eq!(control.snapshot().error, None);
}

#[test]
fn publication_gap_reconciles_and_shutdown_preserves_pause() {
    let temp = tempfile::tempdir().unwrap();
    let control = open(&temp);
    control.publishing().unwrap();
    drop(control);
    let control = open(&temp);
    assert_eq!(control.snapshot().phase, Some(IngestionPhase::Restoring));
    control.publishing().unwrap();
    drop(control);
    let control = ColdControl::open(
        &temp.path().join("index.db"),
        &temp.path().join("index.ledger"),
        true,
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap()
    .unwrap();
    assert_eq!(control.snapshot().state, IngestionState::Succeeded);

    let temp = tempfile::tempdir().unwrap();
    let control = open(&temp);
    let before = control.snapshot();
    command(&control, IngestionAction::Pause, "pause");
    control.stopped(None).unwrap();
    control.shutdown.store(true, Ordering::SeqCst);
    assert!(control.wait().is_err());
    assert_eq!(
        control
            .command(
                &before.id,
                &before.index_id,
                "pause",
                IngestionAction::Pause,
                IngestionControlRequest {
                    expected_revision: before.revision
                }
            )
            .unwrap()
            .run
            .state,
        IngestionState::Paused
    );
}

#[test]
fn concurrent_pause_and_publication_have_exactly_one_winner() {
    for _ in 0..8 {
        let temp = tempfile::tempdir().unwrap();
        let control = open(&temp);
        let run = control.snapshot();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        std::thread::scope(|scope| {
            let publish = scope.spawn(|| {
                barrier.wait();
                control.publishing()
            });
            barrier.wait();
            let pause = control.command(
                &run.id,
                &run.index_id,
                "race",
                IngestionAction::Pause,
                IngestionControlRequest {
                    expected_revision: run.revision,
                },
            );
            assert_ne!(publish.join().unwrap().is_ok(), pause.is_ok());
        });
    }
}

#[test]
fn receipt_capacity_reserves_pause_and_never_evicts_unexpired_keys() {
    let temp = tempfile::tempdir().unwrap();
    let control = open(&temp);
    {
        let mut inner = control.inner.lock().unwrap();
        inner.record.receipts = (0..CAPACITY - 1)
            .map(|i| Receipt {
                key: format!("key-{i}"),
                request: "prior request".into(),
                accepted_at: now_ms(),
            })
            .collect();
    }
    let run = control.snapshot();
    assert_eq!(
        control
            .command(
                &run.id,
                &run.index_id,
                "resume",
                IngestionAction::Resume,
                IngestionControlRequest {
                    expected_revision: run.revision
                }
            )
            .unwrap_err()
            .code,
        ErrorCode::ControlCapacity
    );
    command(&control, IngestionAction::Pause, "reserved-pause");
    control.stopped(None).unwrap();
    let run = control.snapshot();
    assert_eq!(
        control
            .command(
                &run.id,
                &run.index_id,
                "resume",
                IngestionAction::Resume,
                IngestionControlRequest {
                    expected_revision: run.revision
                }
            )
            .unwrap_err()
            .code,
        ErrorCode::ControlCapacity
    );
    assert_eq!(
        control.inner.lock().unwrap().record.receipts.len(),
        CAPACITY
    );
    {
        let mut inner = control.inner.lock().unwrap();
        for receipt in &mut inner.record.receipts {
            receipt.accepted_at = now_ms() - RETAIN_MS - 1;
        }
    }
    assert!(command(&control, IngestionAction::Resume, "resume").accepted);
    assert_eq!(control.inner.lock().unwrap().record.receipts.len(), 1);
}

#[tokio::test]
async fn http_controls_share_status_and_return_typed_noncacheable_results() {
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use rosetta_server::{AppState, DaemonHealth, Hub, HubConfig, router};
    use tower::ServiceExt;
    let temp = tempfile::tempdir().unwrap();
    let control = open(&temp);
    let health = Arc::new(DaemonHealth::new(1).with_ingestion(control.clone()));
    let app = router(
        AppState::new(
            Arc::new(Hub::new(HubConfig::default(), 1)),
            temp.path().join("view.db"),
        )
        .with_daemon_health(health),
    );
    let run = control.snapshot();
    let command = |index: &str, key: &str, body: &str| {
        Request::builder()
            .method("POST")
            .uri(format!("/api/ingestions/{}/pause", run.id))
            .header("content-type", "application/json")
            .header("x-rosetta-index-id", index)
            .header("idempotency-key", key)
            .body(Body::from(body.to_owned()))
            .unwrap()
    };
    let body = format!("{{\"expected_revision\":{}}}", run.revision);
    let key = "00000000-0000-4000-8000-000000000001";
    let wrong = app
        .clone()
        .oneshot(command("wrong-index", key, &body))
        .await
        .unwrap();
    assert_eq!(wrong.status(), 409);
    let error: serde_json::Value =
        serde_json::from_slice(&wrong.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(error["error"]["code"], "index_changed");
    assert_eq!(error["error"]["details"]["current_revision"], run.revision);
    for (key, body) in [
        ("not-a-uuid", body.as_str()),
        (key, "{}"),
        (key, "not json"),
    ] {
        let bad = app
            .clone()
            .oneshot(command(&run.index_id, key, body))
            .await
            .unwrap();
        assert_eq!(bad.status(), 400);
        assert_eq!(bad.headers()["cache-control"], "no-store");
    }
    let reply = app
        .clone()
        .oneshot(command(&run.index_id, key, &body))
        .await
        .unwrap();
    assert_eq!(reply.status(), 202);
    assert_eq!(
        reply.headers()["location"],
        format!("/api/ingestions/{}", run.id)
    );
    assert_eq!(reply.headers()["retry-after"], "1");
    assert_eq!(reply.headers()["cache-control"], "no-store");
    control.stopped(None).unwrap();
    let status = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status: serde_json::Value =
        serde_json::from_slice(&status.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(status["daemon"]["ingest"]["run"]["state"], "paused");
    let reply = app
        .oneshot(command(&run.index_id, key, &body))
        .await
        .unwrap();
    assert_eq!(reply.status(), 200);
}
