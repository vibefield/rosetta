//! The served shell (RO-32) and the publication's request words (RO-33,
//! RO-34a). The shell's shared parts are pinned in `golden/served-shell.json`,
//! which the room's parity test (`clients/app/test/artifact-shell-parity.test.mjs`)
//! reads back against `clients/room/src/components/artifacts/preview.ts`. If
//! the golden test fails, either the shell changed by accident, or bump
//! `SHELL_VERSION` and refresh it with
//! `UPDATE_SHELL_GOLDEN=1 cargo test -p rosetta-core --test served_shell`.

use rosetta_core::html_artifacts::{
    HTML_DESCRIPTION_MAX_BYTES, HtmlArtifactPublication, SHELL_BASE_STYLE, SHELL_HOSTS,
    SHELL_VARIABLES, SHELL_VERSION, identity, runnable_document, shell_policy,
};

const GOLDEN_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/golden/served-shell.json"
);

fn shell() -> serde_json::Value {
    serde_json::json!({
        "shell_version": SHELL_VERSION,
        "hosts": {
            "script": SHELL_HOSTS.script,
            "style": SHELL_HOSTS.style,
            "font": SHELL_HOSTS.font,
            "all": SHELL_HOSTS.all(),
        },
        "policy": shell_policy(),
        "variables": SHELL_VARIABLES
            .iter()
            .map(|(name, value)| serde_json::json!([name, value]))
            .collect::<Vec<_>>(),
        "base_style": SHELL_BASE_STYLE,
    })
}

#[test]
fn the_served_shell_is_pinned_for_the_rooms_parity_test() {
    let rendered = serde_json::to_string_pretty(&shell()).expect("the shell serializes");
    if std::env::var_os("UPDATE_SHELL_GOLDEN").is_some() {
        std::fs::create_dir_all(std::path::Path::new(GOLDEN_PATH).parent().unwrap()).unwrap();
        std::fs::write(GOLDEN_PATH, rendered + "\n").unwrap();
        return;
    }
    let golden = std::fs::read_to_string(GOLDEN_PATH).expect(
        "tests/golden/served-shell.json missing — generate it with \
         UPDATE_SHELL_GOLDEN=1 cargo test -p rosetta-core --test served_shell",
    );
    assert_eq!(
        golden.trim_end(),
        rendered,
        "\nTHE SERVED SHELL CHANGED. Bump SHELL_VERSION (a shell export answers it\n\
         as shell.version), check the room's parity test, and refresh this golden with\n\
         UPDATE_SHELL_GOLDEN=1 cargo test -p rosetta-core --test served_shell\n"
    );
}

#[test]
fn a_runnable_document_has_no_base_and_reaches_only_its_origin_and_the_named_hosts() {
    let page = "<h1>Review</h1><script src=\"app.js\"></script>";
    let document = runnable_document(page);
    assert!(document.starts_with("<!doctype html><html><head><meta charset=\"utf-8\">"));
    assert!(
        document.ends_with(&format!("<body>{page}</body></html>")),
        "the page sits inside body, as the room places it"
    );
    assert!(!document.contains("<base"), "{document}");
    assert!(!document.contains("artifact.invalid"), "{document}");
    assert!(
        document
            .contains("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">"),
        "{document}"
    );
    assert!(document.contains("<meta name=\"referrer\" content=\"no-referrer\">"));
    let policy = shell_policy();
    assert!(document.contains(&format!(
        "<meta http-equiv=\"Content-Security-Policy\" content=\"{}\">",
        policy.join("; ")
    )));
    for directive in [
        "connect-src 'self'",
        "base-uri 'self'",
        "img-src 'self' data: blob:",
        "media-src 'self' data: blob:",
        "frame-src 'none'",
        "worker-src 'none'",
        "form-action 'none'",
    ] {
        assert!(
            policy.iter().any(|d| d == directive),
            "{directive}: {policy:?}"
        );
    }
    for (prefix, hosts) in [
        ("script-src ", SHELL_HOSTS.script),
        ("style-src ", SHELL_HOSTS.style),
        ("font-src ", SHELL_HOSTS.font),
    ] {
        let directive = policy.iter().find(|d| d.starts_with(prefix)).unwrap();
        assert!(directive.contains("'self'"), "{directive}");
        let named: Vec<&str> = directive
            .split(' ')
            .filter_map(|source| source.strip_prefix("https://"))
            .collect();
        assert_eq!(named, hosts, "{directive}");
    }
    assert!(document.contains(":root { color-scheme: light;"));
    for (name, value) in SHELL_VARIABLES {
        assert!(document.contains(&format!(" {name}: {value};")), "{name}");
    }
    assert!(document.contains(SHELL_BASE_STYLE));
    assert!(
        !document.contains("prefers-color-scheme"),
        "light only: no dark values are defined"
    );
    assert_eq!(
        SHELL_HOSTS.all(),
        [
            "cdn.jsdelivr.net",
            "cdnjs.cloudflare.com",
            "unpkg.com",
            "fonts.googleapis.com",
            "fonts.gstatic.com",
        ],
        "every host once, in directive order"
    );
}

/// Every export answers the room's hosts keyed by directive
/// (`environment.hosts`), shell or not. The keys are the shell policy's own
/// directive names, and each names exactly the hosts that directive admits.
#[test]
fn the_hosts_by_directive_are_the_ones_the_shell_policy_admits() {
    let policy = shell_policy();
    let directives = SHELL_HOSTS.directives();
    assert_eq!(
        directives.map(|(directive, _)| directive),
        ["script-src", "style-src", "font-src"]
    );
    for (directive, hosts) in directives {
        let line = policy
            .iter()
            .find(|line| line.starts_with(&format!("{directive} ")))
            .unwrap_or_else(|| panic!("{directive} is in the shell policy"));
        let named: Vec<&str> = line
            .split(' ')
            .filter_map(|source| source.strip_prefix("https://"))
            .collect();
        assert_eq!(named, hosts, "{directive}");
    }
}

fn publication() -> HtmlArtifactPublication {
    HtmlArtifactPublication {
        id: identity(&["publication"]),
        artifact_id: identity(&["artifact"]),
        session_id: "s1".into(),
        timeline_id: "t1".into(),
        title: "Review".into(),
        filename: "review.html".into(),
        path: Some("/w/review.html".into()),
        url: None,
        native_id: None,
        version: None,
        published_at: Some(1),
        source_offset: 0,
        description: None,
        dropped_files: None,
    }
}

#[test]
fn a_publication_keeps_a_bounded_description_and_its_dropped_file_count() {
    let mut row = publication();
    assert_eq!(row.validate(), Ok(()));
    row.description = Some("A checkout redesign to compare".into());
    row.dropped_files = Some(3);
    assert_eq!(row.validate(), Ok(()));
    row.description = Some("é".repeat(HTML_DESCRIPTION_MAX_BYTES / 2));
    assert_eq!(row.validate(), Ok(()), "exactly the bound");
    for refused in [
        String::new(),
        "x".repeat(HTML_DESCRIPTION_MAX_BYTES + 1),
        "a\0b".to_owned(),
    ] {
        row.description = Some(refused);
        assert_eq!(row.validate(), Err("invalid HTML artifact description"));
    }

    // A checkpoint written before schema 30 has neither field.
    let mut legacy = serde_json::to_value(publication()).unwrap();
    let object = legacy.as_object_mut().unwrap();
    object.remove("description");
    object.remove("dropped_files");
    let parsed: HtmlArtifactPublication = serde_json::from_value(legacy).unwrap();
    assert_eq!(parsed, publication());
}
