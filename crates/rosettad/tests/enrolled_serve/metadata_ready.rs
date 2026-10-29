//! Exercise the real HTTP boundary on every serve startup path. The two
//! runtimes share a kind, so readiness cannot accidentally count kinds.

use super::*;

#[test]
fn first_pass_is_reported_on_fresh_resumed_warm_and_metadata_only_starts() {
    for mode in ["fresh", "resumed", "warm", "metadata-only"] {
        let bench = Bench::new();
        if matches!(mode, "resumed" | "warm") {
            assert_success(&bench.serve_once(&[]));
        }
        if mode == "resumed" {
            bench.stage_cold_generation(true);
        }
        let flags: &[&str] = if mode == "metadata-only" {
            &["--stop-after-meta"]
        } else {
            &[]
        };
        let (_child, origin) = spawn_serve_with_flags(&bench, flags);
        let deadline = Instant::now() + Duration::from_secs(30);
        let first = loop {
            let status = http_get(&origin, "/api/status");
            let stamp = &status["daemon"]["ingest"]["metadata_ready_at"];
            if stamp.is_i64() {
                break status;
            }
            assert_eq!(
                status["daemon"]["ingest"].get("metadata_ready_at"),
                Some(&serde_json::Value::Null),
                "{mode}: the pending stamp must be present: {status}"
            );
            assert!(
                Instant::now() < deadline,
                "{mode}: first pass timed out: {status}"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        let stamp = first["daemon"]["ingest"]["metadata_ready_at"]
            .as_i64()
            .unwrap();
        assert!(
            stamp >= first["daemon"]["started_at"].as_i64().unwrap(),
            "{mode}: {first}"
        );
        assert_eq!(
            first["daemon"]["ingest"]["runtimes"]
                .as_array()
                .unwrap()
                .len(),
            2
        );

        // Both runtime catalogs are already public when the stamp is seen.
        let runtimes = http_get(&origin, "/api/runtimes");
        let rows = runtimes["runtimes"].as_array().unwrap();
        assert!(
            rows.iter().any(|row| row["id"] == bench.native_id()),
            "{mode}: {runtimes}"
        );
        assert!(
            rows.iter().any(|row| row["id"] == bench.kim_id()),
            "{mode}: {runtimes}"
        );
        let sessions = http_get(&origin, "/api/sessions");
        let rows = sessions["sessions"].as_array().unwrap();
        assert!(
            rows.iter()
                .any(|row| row["runtime_id"] == bench.native_id()),
            "{mode}: {sessions}"
        );
        assert!(
            rows.iter().any(|row| row["runtime_id"] == bench.kim_id()),
            "{mode}: {sessions}"
        );

        // The original observation survives publication/live catch-up; audit
        // mode stays at metadata_ready without making transcripts ready.
        let final_phase = if mode == "metadata-only" {
            "metadata_ready"
        } else {
            "live"
        };
        loop {
            let status = http_get(&origin, "/api/status");
            assert_eq!(
                status["daemon"]["ingest"]["metadata_ready_at"], stamp,
                "{mode}: {status}"
            );
            if status["daemon"]["ingest"]["phase"] == final_phase {
                assert_eq!(
                    status["canonical_ready"],
                    mode != "metadata-only",
                    "{mode}: {status}"
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{mode}: final phase timed out: {status}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
