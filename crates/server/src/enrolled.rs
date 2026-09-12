//! Serve-time presentation for supervisor-enrolled runtimes (2026-09-12).
//!
//! A supervisor may enroll extra data roots of one kind through the daemon's
//! `--enroll` file. Their labels and their `enrolled` provenance are facts of
//! THIS start's file, never projection columns (iron rule 5), so the mirror's
//! rows carry neither. The transport decorates rows on the way out instead:
//! `/api/runtimes`, the `/ws/meta` snapshot, and the meta stream's runtime
//! mutations all pass through [`EnrolledRuntimes::decorate`]. A runtime the
//! file does not name is left exactly as the mirror holds it — discovered.

use std::collections::BTreeMap;

use rosetta_core::rows::{RuntimeProvenance, RuntimeRow};

#[derive(Debug, Default)]
pub struct EnrolledRuntimes {
    /// Runtime id → the file's label for it (which may be absent).
    labels: BTreeMap<String, Option<String>>,
}

impl EnrolledRuntimes {
    pub fn new(entries: impl IntoIterator<Item = (String, Option<String>)>) -> Self {
        Self {
            labels: entries.into_iter().collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.labels.is_empty()
    }

    pub fn contains(&self, runtime_id: &str) -> bool {
        self.labels.contains_key(runtime_id)
    }

    /// Fill `label` and `provenance` on an enrolled row; discovered rows are
    /// untouched.
    pub fn decorate(&self, row: &mut RuntimeRow) {
        if let Some(label) = self.labels.get(&row.id) {
            row.label = label.clone();
            row.provenance = RuntimeProvenance::Enrolled;
        }
    }

    pub fn decorate_all(&self, rows: &mut [RuntimeRow]) {
        if self.labels.is_empty() {
            return;
        }
        for row in rows {
            self.decorate(row);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str) -> RuntimeRow {
        RuntimeRow {
            id: id.into(),
            kind: "claude-code".into(),
            data_root: "~/.claude".into(),
            enabled: true,
            account: None,
            config: None,
            skills: None,
            versions_seen: None,
            discovered_at: 1,
            label: None,
            provenance: Default::default(),
        }
    }

    #[test]
    fn only_enrolled_rows_are_decorated() {
        let enrolled = EnrolledRuntimes::new([
            ("rt-kim".to_string(), Some("Kim".to_string())),
            ("rt-anon".to_string(), None),
        ]);
        let mut rows = vec![row("rt-native"), row("rt-kim"), row("rt-anon")];
        enrolled.decorate_all(&mut rows);
        assert_eq!(rows[0].provenance, RuntimeProvenance::Discovered);
        assert_eq!(rows[0].label, None);
        assert_eq!(rows[1].provenance, RuntimeProvenance::Enrolled);
        assert_eq!(rows[1].label.as_deref(), Some("Kim"));
        assert_eq!(rows[2].provenance, RuntimeProvenance::Enrolled);
        assert_eq!(rows[2].label, None);

        // The wire omits both fields for a discovered row and carries them
        // for an enrolled one — the serde shape older clients parse.
        let native = serde_json::to_value(&rows[0]).unwrap();
        assert!(native.get("label").is_none() && native.get("provenance").is_none());
        let kim = serde_json::to_value(&rows[1]).unwrap();
        assert_eq!(kim["label"], "Kim");
        assert_eq!(kim["provenance"], "enrolled");
    }
}
