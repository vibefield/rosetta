//! Folder admission is session-wide, settled before any metadata is journaled.
//! A sibling with no cwd must not bypass another sibling's exclusion. Children
//! must not recreate an excluded ancestor as a public placeholder, either.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use rosetta_adapter_support::SkimResult;
use rosetta_core::{ids, rows::SessionRow};
use rosetta_store::{Store, queries};

use super::MetaOutput;
use crate::{CwdExclusions, PipelineError};

/// Older disk cold loads can already hold an unfinished source when a new
/// start excludes it. Retain its rows/fence, but stop waiting for its capture
/// at publication. Sources absent from today's walk use retained metadata.
pub(super) fn retire_interrupted_sources(
    runtime_id: &str,
    store: &mut Store,
    policy: &CwdExclusions,
    excluded: &BTreeSet<String>,
) -> Result<(), PipelineError> {
    if policy.is_empty() {
        return Ok(());
    }
    for (cursor, session, checkpoint) in queries::incomplete_cursors(store.conn(), runtime_id)? {
        let Some(session) = session.or_else(|| {
            crate::PipeCheckpoint::decode(&checkpoint).map(|checkpoint| checkpoint.session_id)
        }) else {
            continue;
        };
        if excluded.contains(&session) || policy.excludes_session(store, &session)? {
            store.retire_source(cursor);
        }
    }
    Ok(())
}

#[derive(Debug, Default)]
pub(super) struct CwdAdmission {
    policy: CwdExclusions,
    excluded: BTreeSet<String>,
    pub(super) parents: BTreeMap<String, BTreeSet<String>>,
}

impl CwdAdmission {
    pub(super) fn new(policy: CwdExclusions) -> Self {
        Self {
            policy,
            ..Self::default()
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.policy.is_empty()
    }

    pub(super) fn excludes_cwd(&self, cwd: Option<&str>) -> bool {
        cwd.is_some_and(|cwd| self.policy.excludes(cwd))
    }

    pub(super) fn observe(&mut self, runtime: &str, id: &str, skim: &SkimResult) {
        if self.is_empty() {
            return;
        }
        if self.excludes_cwd(skim.cwd.as_deref()) {
            self.excluded.insert(id.to_owned());
        }
        // Include ledger-only internal sources: a user exclusion stops raw
        // capture too, unlike the adapter's own internal-session policy.
        self.parents.entry(id.to_owned()).or_default().extend(
            [
                skim.parent_session_id.as_deref(),
                skim.root_session_id.as_deref(),
            ]
            .into_iter()
            .flatten()
            .map(|native| ids::session_id(runtime, native))
            .filter(|parent| parent != id),
        );
    }

    pub(super) fn observe_row(&mut self, row: &SessionRow) {
        if self.excludes_cwd(row.cwd.as_deref()) {
            self.excluded.insert(row.id.clone());
        }
        self.parents.entry(row.id.clone()).or_default().extend(
            [row.parent_session_id.as_ref(), Some(&row.root_session_id)]
                .into_iter()
                .flatten()
                .filter(|parent| *parent != &row.id)
                .cloned(),
        );
    }

    pub(super) fn filter(
        &mut self,
        output: &mut MetaOutput,
        store: &Store,
    ) -> Result<BTreeSet<String>, PipelineError> {
        if self.is_empty() {
            return Ok(BTreeSet::new());
        }
        for row in &output.sessions {
            self.observe_row(row);
        }
        // The parent transcript or cwd-bearing sidecar may have been pruned.
        // Retained metadata is still proof of that folder and relationship.
        // Read each identity once, including ancestors absent from this walk.
        let mut pending: Vec<_> = self.parents.keys().cloned().collect();
        let mut visited = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !visited.insert(id.clone()) {
                continue;
            }
            if let Some(row) = queries::session(store.conn(), &id)? {
                self.observe_row(&row);
            }
            if let Some(parents) = self.parents.get(&id) {
                pending.extend(parents.iter().filter(|id| !visited.contains(*id)).cloned());
            }
        }

        let mut children: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (id, parents) in &self.parents {
            for parent in parents {
                children.entry(parent).or_default().push(id);
            }
        }
        let mut pending: VecDeque<_> = self.excluded.iter().cloned().collect();
        while let Some(id) = pending.pop_front() {
            for child in children.get(id.as_str()).into_iter().flatten() {
                if self.excluded.insert((*child).to_owned()) {
                    pending.push_back((*child).to_owned());
                }
            }
        }
        if self.excluded.is_empty() {
            return Ok(self.excluded.clone());
        }

        output
            .files
            .retain(|file| !self.excluded.contains(&file.session_id));

        // Keep placeholders only when an admitted public source needs them.
        // An excluded child must not leave a phantom parent/project behind.
        let mut needed = BTreeSet::new();
        let mut pending: Vec<_> = output
            .files
            .iter()
            .filter(|file| !file.ledger_only)
            .map(|file| file.session_id.clone())
            .collect();
        while let Some(id) = pending.pop() {
            if self.excluded.contains(&id) || !needed.insert(id.clone()) {
                continue;
            }
            if let Some(parents) = self.parents.get(&id) {
                pending.extend(parents.iter().cloned());
            }
        }
        output.sessions.retain(|row| needed.contains(&row.id));
        output.session_positions = output
            .sessions
            .iter()
            .enumerate()
            .map(|(index, row)| (row.id.clone(), index))
            .collect();
        output.project_hints.retain(|id, _| needed.contains(id));
        output
            .ops
            .retain(|(owner, _)| owner.as_ref().is_none_or(|id| needed.contains(id)));
        // Store and ledger rows are intentionally untouched. This is retirement
        // of capture; archived history remains readable and rebuildable.
        Ok(self.excluded.clone())
    }
}
