use super::*;

fn session(
    id: &str,
    cwd: Option<&str>,
    parent: Option<&str>,
    root: &str,
    role: SessionRole,
    started_at: i64,
) -> SessionRow {
    SessionRow {
        id: id.to_owned(),
        native_session_id: id.to_owned(),
        runtime_id: "runtime".to_owned(),
        project_id: None,
        cwd: cwd.map(str::to_owned),
        parent_session_id: parent.map(str::to_owned),
        delegation: None,
        root_session_id: root.to_owned(),
        role,
        title: None,
        summary: None,
        started_at,
        updated_at: started_at,
        activity: None,
        stream_state: StreamState::Pending,
    }
}

fn merged(rows: impl IntoIterator<Item = SessionRow>) -> Vec<SessionRow> {
    let mut output = MetaOutput::default();
    for row in rows {
        output.merge_session(row);
    }
    output.sessions
}

#[test]
fn metadata_cwd_selection_is_order_independent() {
    let early = session("s", Some("/work/early"), None, "s", SessionRole::Main, 10);
    let late = session("s", Some("/work/late"), None, "s", SessionRole::Main, 20);
    let left = merged([early.clone(), late.clone()]);
    let right = merged([late, early]);
    assert_eq!(left[0].cwd.as_deref(), Some("/work/early"));
    assert_eq!(left[0].cwd, right[0].cwd);
}

#[test]
fn indexed_merges_fold_each_session_alone_in_first_seen_order() {
    let mut rows = Vec::new();
    for round in 0..5_i64 {
        for id in 0..40_i64 {
            let mut row = session(
                &format!("s{id}"),
                Some(&format!("/w/{}", (id + round) % 7)),
                (round % 2 == 1).then_some("p"),
                &format!("s{id}"),
                if round == 3 {
                    SessionRole::Subagent
                } else {
                    SessionRole::Main
                },
                1_000 - round * 10 + id,
            );
            row.updated_at = row.started_at + round;
            row.title = (round == id % 5).then(|| format!("t{round}"));
            rows.push(row);
        }
    }
    // A fixed interleaving, so sessions are first seen out of id order.
    let mut position = 0_usize;
    for index in (1..rows.len()).rev() {
        position = (position * 31 + 17) % (index + 1);
        rows.swap(index, position);
    }

    let mut output = MetaOutput::default();
    for row in rows.clone() {
        output.merge_session(row);
    }
    let mut first_seen: Vec<&str> = Vec::new();
    for row in &rows {
        if !first_seen.contains(&row.id.as_str()) {
            first_seen.push(&row.id);
        }
    }
    let ids: Vec<&str> = output.sessions.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(ids, first_seen);
    for (index, session) in output.sessions.iter().enumerate() {
        assert_eq!(output.session_positions[&session.id], index);
        let alone = merged(rows.iter().filter(|row| row.id == session.id).cloned());
        assert_eq!(alone, std::slice::from_ref(session));
    }
}

#[test]
fn exact_sidecar_parent_outranks_directory_fallback_in_any_order() {
    let fallback = session(
        "child",
        None,
        Some("root"),
        "root",
        SessionRole::Subagent,
        10,
    );
    let mut exact = fallback.clone();
    exact.parent_session_id = Some("worker-parent".into());
    exact.delegation = Some(rosetta_core::SessionDelegation {
        parent_tool_native_id: Some("toolu-child".into()),
        workflow_run_id: None,
        team: None,
        color: None,
        configured_model: Some("opus".into()),
    });
    let forward = merged([fallback.clone(), exact.clone()]);
    let reverse = merged([exact, fallback]);
    assert_eq!(forward, reverse);
    assert_eq!(
        forward[0].parent_session_id.as_deref(),
        Some("worker-parent")
    );
    assert_eq!(
        forward[0]
            .delegation
            .as_ref()
            .and_then(|delegation| delegation.parent_tool_native_id.as_deref()),
        Some("toolu-child")
    );
}

#[test]
fn child_without_cwd_inherits_only_from_metadata_parent() {
    let mut output = MetaOutput {
        sessions: vec![
            session(
                "parent",
                Some("/work/project"),
                None,
                "parent",
                SessionRole::Main,
                10,
            ),
            session(
                "child",
                None,
                Some("parent"),
                "parent",
                SessionRole::Subagent,
                20,
            ),
        ],
        ..MetaOutput::default()
    };
    settle_projects(&mut output);
    assert_eq!(output.sessions[1].cwd.as_deref(), Some("/work/project"));
    assert_eq!(output.sessions[1].project_id, output.sessions[0].project_id);
    assert_eq!(output.projects.len(), 1);
}

#[test]
fn missing_metadata_stays_unassigned() {
    let mut output = MetaOutput {
        sessions: vec![session("s", None, None, "s", SessionRole::Main, 10)],
        ..MetaOutput::default()
    };
    settle_projects(&mut output);
    assert_eq!(output.sessions[0].cwd, None);
    assert_eq!(output.sessions[0].project_id, None);
    assert!(output.projects.is_empty());
}

#[test]
fn native_owner_links_a_deleted_worktree_to_the_parent_project() {
    let parent_cwd = "/work/original";
    let child_cwd = "/gone/worktrees/child";
    let mut output = MetaOutput {
        sessions: vec![
            session(
                "parent",
                Some(parent_cwd),
                None,
                "parent",
                SessionRole::Main,
                10,
            ),
            session(
                "child",
                Some(child_cwd),
                Some("parent"),
                "parent",
                SessionRole::Subagent,
                20,
            ),
        ],
        project_hints: BTreeMap::from([(
            "child".to_owned(),
            BTreeSet::from([ProjectHint::SameAsSession {
                native_session_id: "parent".to_owned(),
            }]),
        )]),
        ..MetaOutput::default()
    };

    settle_projects(&mut output);

    assert_eq!(output.sessions[1].project_id, output.sessions[0].project_id);
    assert_eq!(output.projects.len(), 1);
    assert_eq!(output.projects[0].name, "original");
    assert_eq!(
        output.projects[0].root_paths,
        vec![child_cwd.to_owned(), parent_cwd.to_owned()]
    );
}

#[test]
fn native_checkout_root_links_a_deleted_worktree_without_live_git_metadata() {
    let canonical_root = "/work/original";
    let child_cwd = "/gone/worktrees/child";
    let mut output = MetaOutput {
        sessions: vec![session(
            "child",
            Some(child_cwd),
            None,
            "child",
            SessionRole::Main,
            20,
        )],
        project_hints: BTreeMap::from([(
            "child".to_owned(),
            BTreeSet::from([ProjectHint::Root {
                root: canonical_root.to_owned(),
                git_repo: Some("git@example.com:acme/repo.git".to_owned()),
            }]),
        )]),
        ..MetaOutput::default()
    };

    settle_projects(&mut output);

    assert_eq!(output.sessions[0].cwd.as_deref(), Some(child_cwd));
    assert_eq!(output.projects.len(), 1);
    assert_eq!(
        output.sessions[0].project_id.as_deref(),
        Some(ids::project_id(canonical_root, None).as_str())
    );
    assert_eq!(output.projects[0].name, "original");
    assert_eq!(
        output.projects[0].root_paths,
        vec![child_cwd.to_owned(), canonical_root.to_owned()]
    );
    assert_eq!(
        output.projects[0].git_repo.as_deref(),
        Some("example.com:acme/repo")
    );
}
