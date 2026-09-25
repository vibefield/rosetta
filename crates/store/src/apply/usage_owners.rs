//! Cross-file usage ownership (ARCHITECTURE §4.1.3, counting rule 2).
//!
//! A model response copied into another transcript — a Claude Code subagent
//! or compaction transcript replaying the parent's history, a fork carrying
//! its origin's rows — keeps its timeline role but is not usage a second
//! time. Adapters stamp every usage op of a response with the response's
//! native identity; this process-local index keeps one owner per identity,
//! the earliest-started session (then the lower session id), and tells the
//! apply path to drop or retract the other copies' usage. Like the RAM
//! reconciliation buffer it is never persisted: a rebuild replays the raw
//! ledger through it, and a daemon that opens a complete projection starts
//! empty, so only copies it sees arrive after their owner are caught until
//! the next rebuild.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, params};

use crate::StoreError;

/// Where a claimed usage record lives in the projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OwnerTarget {
    /// A timeline row's usage columns (`timeline_id`).
    Row(String),
    /// A `session_usage` sample (`sample_key`).
    Sample(String),
}

#[derive(Debug)]
struct Owner {
    session: u32,
    target: OwnerTarget,
}

/// The verdict for one usage op carrying an identity.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Claim {
    /// Apply as usual: the record is the owner (first seen, a correction of
    /// the owner, or a retarget within the owning session).
    Own,
    /// A copy of usage another session already owns: apply nothing.
    DropCopy,
    /// The record belongs to an earlier-started session than the current
    /// owner: apply it and retract the previous owner's usage.
    Displace {
        session_id: String,
        target: OwnerTarget,
    },
}

#[derive(Debug, Default)]
pub(crate) struct UsageOwners {
    owners: BTreeMap<String, Owner>,
    sessions: Vec<String>,
    session_index: BTreeMap<String, u32>,
    /// `session.started_at` per interned session; `None` when the row was
    /// not there yet, which ranks after every dated session.
    started: Vec<Option<i64>>,
}

impl UsageOwners {
    /// Decide whether the usage of `identity` arriving for `session_id` is
    /// the owner's, a copy to drop, or displaces the current owner. A
    /// retraction (`carries_usage == false`: the all-None snapshot that
    /// blanks a sample once the adapter moved the fact onto a row) never
    /// becomes the owner's target, so a later displacement retracts where
    /// the usage actually lives.
    pub(crate) fn claim(
        &mut self,
        conn: &Connection,
        identity: &str,
        session_id: &str,
        target: OwnerTarget,
        carries_usage: bool,
    ) -> Result<Claim, StoreError> {
        let session = self.intern(conn, session_id)?;
        let Some(owner) = self.owners.get_mut(identity) else {
            self.owners
                .insert(identity.to_owned(), Owner { session, target });
            return Ok(Claim::Own);
        };
        if owner.session == session {
            if carries_usage {
                owner.target = target;
            }
            return Ok(Claim::Own);
        }
        let claimant = (
            self.started[session as usize],
            &self.sessions[session as usize],
        );
        let current = (
            self.started[owner.session as usize],
            &self.sessions[owner.session as usize],
        );
        // `None` (undated) sorts after `Some`, so a dated session wins over
        // one whose metadata has not landed; equal dates fall to the id.
        let claimant_first = match (claimant.0, current.0) {
            (Some(a), Some(b)) if a != b => a < b,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            _ => claimant.1 < current.1,
        };
        if !claimant_first {
            return Ok(Claim::DropCopy);
        }
        let displaced = Claim::Displace {
            session_id: self.sessions[owner.session as usize].clone(),
            target: std::mem::replace(&mut owner.target, target),
        };
        owner.session = session;
        Ok(displaced)
    }

    fn intern(&mut self, conn: &Connection, session_id: &str) -> Result<u32, StoreError> {
        if let Some(index) = self.session_index.get(session_id) {
            let index = *index;
            if self.started[index as usize].is_none() {
                self.started[index as usize] = session_started(conn, session_id)?;
            }
            return Ok(index);
        }
        let index =
            u32::try_from(self.sessions.len()).map_err(|_| StoreError::ProjectionInvalid {
                message: "more sessions than the usage owner index can count".to_string(),
            })?;
        self.sessions.push(session_id.to_owned());
        self.session_index.insert(session_id.to_owned(), index);
        self.started.push(session_started(conn, session_id)?);
        Ok(index)
    }
}

fn session_started(conn: &Connection, session_id: &str) -> Result<Option<i64>, StoreError> {
    Ok(conn
        .prepare_cached("SELECT started_at FROM session WHERE id = ?1")?
        .query_row(params![session_id], |row| row.get::<_, i64>(0))
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, started_at INTEGER NOT NULL);
             INSERT INTO session VALUES ('main', 100), ('sub', 200), ('sub2', 200);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn the_earliest_started_session_owns_a_response_whatever_the_arrival_order() {
        let conn = store();
        let mut owners = UsageOwners::default();
        let row = |id: &str| OwnerTarget::Row(id.to_owned());
        // The copy lands first, then the origin displaces it.
        assert_eq!(
            owners
                .claim(&conn, "m1:r1", "sub", row("s-1"), true)
                .unwrap(),
            Claim::Own
        );
        assert_eq!(
            owners
                .claim(&conn, "m1:r1", "main", row("t-1"), true)
                .unwrap(),
            Claim::Displace {
                session_id: "sub".to_owned(),
                target: row("s-1"),
            }
        );
        // A later copy of an owned response is dropped; the owner's own
        // corrections and retargets still apply.
        assert_eq!(
            owners
                .claim(&conn, "m1:r1", "sub2", row("s2-1"), true)
                .unwrap(),
            Claim::DropCopy
        );
        assert_eq!(
            owners
                .claim(
                    &conn,
                    "m1:r1",
                    "main",
                    OwnerTarget::Sample("response:m1".to_owned()),
                    true,
                )
                .unwrap(),
            Claim::Own
        );
        // Equal start times fall to the session id.
        assert_eq!(
            owners
                .claim(&conn, "m2:r2", "sub2", row("x"), true)
                .unwrap(),
            Claim::Own
        );
        assert!(matches!(
            owners.claim(&conn, "m2:r2", "sub", row("y"), true).unwrap(),
            Claim::Displace { .. }
        ));
        // A retraction never moves the owner's target off the usage.
        let sample = || OwnerTarget::Sample("response:m2".to_owned());
        assert_eq!(
            owners
                .claim(&conn, "m3:r3", "main", sample(), true)
                .unwrap(),
            Claim::Own
        );
        assert_eq!(
            owners
                .claim(&conn, "m3:r3", "main", row("t-3"), true)
                .unwrap(),
            Claim::Own
        );
        assert_eq!(
            owners
                .claim(&conn, "m3:r3", "main", sample(), false)
                .unwrap(),
            Claim::Own
        );
        assert_eq!(owners.owners["m3:r3"].target, row("t-3"));
        // An unknown session (no metadata yet) never displaces a dated one.
        assert_eq!(
            owners
                .claim(&conn, "m2:r2", "ghost", row("z"), true)
                .unwrap(),
            Claim::DropCopy
        );
    }
}
