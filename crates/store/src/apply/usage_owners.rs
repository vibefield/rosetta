//! Cross-file usage ownership (ARCHITECTURE §4.1.3, counting rule 2).
//!
//! A model response copied into another transcript — a Claude Code subagent
//! or compaction transcript replaying the parent's history, a fork carrying
//! its origin's rows — keeps its timeline role but is not usage a second
//! time. Adapters stamp every usage op of a response with the response's
//! native identity; this process-local index keeps one owner per identity,
//! the earliest-started session (then the lower session id), and tells the
//! apply path to drop the other copies' usage. Like the RAM reconciliation
//! buffer it is never persisted: a rebuild replays the raw ledger through
//! it, and a daemon that opens a complete projection starts empty, so only
//! copies it sees arrive after their owner are caught until the next rebuild.
//!
//! The verdict is order-free. Each owner remembers every target its records
//! wrote (a timeline row's usage columns, a `session_usage` sample), so when
//! an earlier-started session's copy lands, every one of them is undone: a
//! row loses its usage and gets back the model and time it had before the
//! usage filled them in, a sample row is deleted. Every non-owner copy then
//! ends exactly as if its usage had been dropped on arrival, whichever order
//! the sessions' sources came in. That is what lets independent trees land
//! in any interleaving and still converge (R1, decision D6).

use std::collections::BTreeMap;
use std::mem::size_of;

use rosetta_core::rows::Ms;
use rusqlite::{Connection, OptionalExtension, params};

use crate::StoreError;

/// An owner's position in the index, handed back by [`UsageOwners::claim`]
/// so the apply path records what it wrote without a second identity lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OwnerSlot(u32);

/// The verdict for one usage op carrying an identity.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Claim {
    /// Apply as usual: the record is the owner's (its identity's first
    /// record, or another record of the owning session).
    Own(OwnerSlot),
    /// A copy of usage another session already owns: apply nothing.
    DropCopy,
    /// The record belongs to an earlier-started session than the current
    /// owner: undo every target the previous owner wrote, then apply it.
    Displace {
        slot: OwnerSlot,
        session_id: String,
        undo: Vec<Undo>,
    },
}

/// One target a displaced owner's records wrote, and how to undo it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Undo {
    /// Null the row's usage columns. With `restore`, the usage also moved
    /// the row's model or time: put back `prior`, but only while the row
    /// still holds `written` (a later message upsert that rewrote them is
    /// what a dropped copy would show, so it stays).
    Row {
        timeline_id: String,
        restore: Option<(RowStamp<String>, RowStamp<String>)>,
    },
    /// Delete the `session_usage` sample: a dropped copy never inserts it.
    Sample { key: String },
}

/// A timeline row's model and time, the two columns usage may fill in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RowStamp<M> {
    pub(crate) model: Option<M>,
    pub(crate) ts: Option<Ms>,
}

impl RowStamp<String> {
    pub(crate) fn as_ref(&self) -> RowStamp<&str> {
        RowStamp {
            model: self.model.as_deref(),
            ts: self.ts,
        }
    }
}

/// Interned model name. Usage that changes a row's stamp is rare, and model
/// names repeat across every response, so priors keep an index, not a copy.
type ModelId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    model: Option<ModelId>,
    ts: Option<Ms>,
}

/// What the owner's usage changed about a row's stamp: `prior` is what the
/// row showed before (what a dropped copy shows), `written` what the usage
/// left. Kept only for rows the usage actually changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Restore {
    prior: Stamp,
    written: Stamp,
}

#[derive(Debug)]
enum Touched {
    Row {
        timeline_id: Box<str>,
        restore: Option<Box<Restore>>,
    },
    Sample {
        key: Box<str>,
    },
}

impl Touched {
    fn heap_bytes(&self) -> usize {
        match self {
            Self::Row {
                timeline_id,
                restore,
            } => timeline_id.len() + restore.as_ref().map_or(0, |_| size_of::<Restore>()),
            Self::Sample { key } => key.len(),
        }
    }
}

/// An owner's touched targets. Almost every identity writes one target
/// (its response row), so the common case stays inline.
#[derive(Debug, Default)]
enum TouchedSet {
    #[default]
    Empty,
    One(Touched),
    Many(Vec<Touched>),
}

impl TouchedSet {
    fn iter_mut(&mut self) -> std::slice::IterMut<'_, Touched> {
        match self {
            Self::Empty => [].iter_mut(),
            Self::One(touched) => std::slice::from_mut(touched).iter_mut(),
            Self::Many(all) => all.iter_mut(),
        }
    }

    fn push(&mut self, touched: Touched) -> usize {
        match std::mem::take(self) {
            Self::Empty => {
                *self = Self::One(touched);
                0
            }
            Self::One(first) => {
                *self = Self::Many(vec![first, touched]);
                2 * size_of::<Touched>()
            }
            Self::Many(mut all) => {
                all.push(touched);
                *self = Self::Many(all);
                size_of::<Touched>()
            }
        }
    }

    /// Every target, with the bytes the set held beyond its inline slot.
    fn drain(self) -> (Vec<Touched>, usize) {
        match self {
            Self::Empty => (Vec::new(), 0),
            Self::One(touched) => {
                let bytes = touched.heap_bytes();
                (vec![touched], bytes)
            }
            Self::Many(all) => {
                let bytes = all
                    .iter()
                    .map(|touched| touched.heap_bytes() + size_of::<Touched>())
                    .sum();
                (all, bytes)
            }
        }
    }
}

#[derive(Debug)]
struct Owner {
    session: u32,
    touched: TouchedSet,
}

#[derive(Debug, Default)]
pub(crate) struct UsageOwners {
    index: BTreeMap<Box<str>, u32>,
    owners: Vec<Owner>,
    sessions: Vec<String>,
    session_index: BTreeMap<String, u32>,
    /// `session.started_at` per interned session; `None` when the row was
    /// not there yet, which ranks after every dated session.
    started: Vec<Option<i64>>,
    models: Vec<Box<str>>,
    model_index: BTreeMap<Box<str>, ModelId>,
    /// Heap held by identity keys, session and model names, and touched
    /// targets: the variable part of [`UsageOwners::approx_bytes`].
    heap_bytes: usize,
}

/// Rough per-entry cost of a `BTreeMap` slot beyond its key and value:
/// node headers and the unused part of an 11-slot node, amortized.
const MAP_ENTRY_OVERHEAD: usize = 16;

impl UsageOwners {
    /// Decide whether the usage of `identity` arriving for `session_id` is
    /// the owner's, a copy to drop, or displaces the current owner.
    pub(crate) fn claim(
        &mut self,
        conn: &Connection,
        identity: &str,
        session_id: &str,
    ) -> Result<Claim, StoreError> {
        let session = self.intern_session(conn, session_id)?;
        let Some(&slot) = self.index.get(identity) else {
            let slot = u32::try_from(self.owners.len()).map_err(|_| too_many("identities"))?;
            self.index.insert(identity.into(), slot);
            self.heap_bytes += identity.len();
            self.owners.push(Owner {
                session,
                touched: TouchedSet::Empty,
            });
            return Ok(Claim::Own(OwnerSlot(slot)));
        };
        let current = self.owners[slot as usize].session;
        if current == session {
            return Ok(Claim::Own(OwnerSlot(slot)));
        }
        // An owner that claimed before its session row landed ranks by the
        // row once it is there, whichever session asks next.
        if self.started[current as usize].is_none() {
            self.started[current as usize] =
                session_started(conn, &self.sessions[current as usize])?;
        }
        if !self.precedes(session, current) {
            return Ok(Claim::DropCopy);
        }
        let owner = &mut self.owners[slot as usize];
        owner.session = session;
        let (touched, bytes) = std::mem::take(&mut owner.touched).drain();
        self.heap_bytes -= bytes;
        let undo = touched
            .into_iter()
            .map(|touched| match touched {
                Touched::Row {
                    timeline_id,
                    restore,
                } => Undo::Row {
                    timeline_id: timeline_id.into(),
                    restore: restore.map(|restore| {
                        (
                            self.stamp_text(restore.prior),
                            self.stamp_text(restore.written),
                        )
                    }),
                },
                Touched::Sample { key } => Undo::Sample { key: key.into() },
            })
            .collect();
        Ok(Claim::Displace {
            slot: OwnerSlot(slot),
            session_id: self.sessions[current as usize].clone(),
            undo,
        })
    }

    /// Who wins between two claimants: the dated session before an undated
    /// one, the earlier start, then the lower session id.
    fn precedes(&self, claimant: u32, current: u32) -> bool {
        let (claimant_at, current_at) = (
            self.started[claimant as usize],
            self.started[current as usize],
        );
        match (claimant_at, current_at) {
            (Some(a), Some(b)) if a != b => a < b,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            _ => self.sessions[claimant as usize] < self.sessions[current as usize],
        }
    }

    /// Record that the owner's usage wrote a `session_usage` sample.
    pub(crate) fn touch_sample(&mut self, slot: OwnerSlot, key: &str) {
        let owner = &mut self.owners[slot.0 as usize];
        let known = owner
            .touched
            .iter_mut()
            .any(|touched| matches!(touched, Touched::Sample { key: known } if &**known == key));
        if !known {
            let bytes = owner.touched.push(Touched::Sample { key: key.into() });
            self.heap_bytes += bytes + key.len();
        }
    }

    /// Record that the owner's usage wrote a timeline row whose stamp went
    /// from `old` to `next`. The first change remembers the row's prior
    /// stamp; a later write by the same owner keeps that prior while the row
    /// still shows what the owner last wrote, and otherwise (a message
    /// upsert rewrote the stamp in between) takes the rewritten stamp as the
    /// new prior, which is what a dropped copy would show. An upsert that
    /// writes back exactly the stamp the usage wrote cannot be told apart
    /// and is undone too; a replayed record repeats its own first stamp, and
    /// Claude Code's response rows already carry the usage's model and time,
    /// so neither case records a prior at all.
    pub(crate) fn touch_row(
        &mut self,
        slot: OwnerSlot,
        timeline_id: &str,
        old: RowStamp<&str>,
        next: RowStamp<&str>,
    ) {
        let changed = old != next;
        let Some(position) = self.row_position(slot, timeline_id) else {
            let restore = changed.then(|| {
                Box::new(Restore {
                    prior: self.stamp(old),
                    written: self.stamp(next),
                })
            });
            let held = restore.as_ref().map_or(0, |_| size_of::<Restore>());
            let pushed = self.owners[slot.0 as usize].touched.push(Touched::Row {
                timeline_id: timeline_id.into(),
                restore,
            });
            self.heap_bytes += pushed + held + timeline_id.len();
            return;
        };
        let updated = match self.restore_at(slot, position).as_deref().copied() {
            None if !changed => return,
            None => Restore {
                prior: self.stamp(old),
                written: self.stamp(next),
            },
            Some(held) => {
                let old = self.stamp(old);
                Restore {
                    prior: if old == held.written { held.prior } else { old },
                    written: self.stamp(next),
                }
            }
        };
        let keep = updated.prior != updated.written;
        let restore = self.restore_at(slot, position);
        let had = restore.is_some();
        *restore = keep.then(|| Box::new(updated));
        match (had, keep) {
            (false, true) => self.heap_bytes += size_of::<Restore>(),
            (true, false) => self.heap_bytes -= size_of::<Restore>(),
            _ => {}
        }
    }

    fn row_position(&mut self, slot: OwnerSlot, timeline_id: &str) -> Option<usize> {
        self.owners[slot.0 as usize]
            .touched
            .iter_mut()
            .position(|touched| {
                matches!(touched, Touched::Row { timeline_id: known, .. } if &**known == timeline_id)
            })
    }

    fn restore_at(&mut self, slot: OwnerSlot, position: usize) -> &mut Option<Box<Restore>> {
        match self.owners[slot.0 as usize]
            .touched
            .iter_mut()
            .nth(position)
        {
            Some(Touched::Row { restore, .. }) => restore,
            _ => unreachable!("the position names a row target"),
        }
    }

    /// The session currently owning `identity`'s usage.
    pub(crate) fn owner_of(&self, identity: &str) -> Option<&str> {
        let slot = *self.index.get(identity)?;
        Some(&self.sessions[self.owners[slot as usize].session as usize])
    }

    /// Approximate resident bytes of the index: the size every claim and
    /// touched target adds, for the cold-build memory evidence.
    pub(crate) fn approx_bytes(&self) -> u64 {
        let identities = self.owners.capacity() * size_of::<Owner>()
            + self.index.len() * (size_of::<Box<str>>() + size_of::<u32>() + MAP_ENTRY_OVERHEAD);
        let sessions = self.sessions.capacity() * size_of::<String>()
            + self.started.capacity() * size_of::<Option<i64>>()
            + self.session_index.len()
                * (size_of::<String>() + size_of::<u32>() + MAP_ENTRY_OVERHEAD);
        let models = self.models.capacity() * size_of::<Box<str>>()
            + self.model_index.len()
                * (size_of::<Box<str>>() + size_of::<ModelId>() + MAP_ENTRY_OVERHEAD);
        u64::try_from(identities + sessions + models + self.heap_bytes).unwrap_or(u64::MAX)
    }

    fn stamp(&mut self, stamp: RowStamp<&str>) -> Stamp {
        Stamp {
            model: stamp.model.map(|model| self.intern_model(model)),
            ts: stamp.ts,
        }
    }

    fn stamp_text(&self, stamp: Stamp) -> RowStamp<String> {
        RowStamp {
            model: stamp
                .model
                .map(|model| self.models[model as usize].to_string()),
            ts: stamp.ts,
        }
    }

    fn intern_model(&mut self, model: &str) -> ModelId {
        if let Some(id) = self.model_index.get(model) {
            return *id;
        }
        // Model names are a handful per corpus; running out of u32 ids would
        // take billions of distinct names.
        let id = ModelId::try_from(self.models.len()).expect("model names fit a u32 index");
        self.models.push(model.into());
        self.model_index.insert(model.into(), id);
        self.heap_bytes += 2 * model.len();
        id
    }

    fn intern_session(&mut self, conn: &Connection, session_id: &str) -> Result<u32, StoreError> {
        if let Some(index) = self.session_index.get(session_id) {
            let index = *index;
            if self.started[index as usize].is_none() {
                self.started[index as usize] = session_started(conn, session_id)?;
            }
            return Ok(index);
        }
        let index = u32::try_from(self.sessions.len()).map_err(|_| too_many("sessions"))?;
        self.sessions.push(session_id.to_owned());
        self.session_index.insert(session_id.to_owned(), index);
        self.started.push(session_started(conn, session_id)?);
        self.heap_bytes += 2 * session_id.len();
        Ok(index)
    }
}

fn too_many(what: &str) -> StoreError {
    StoreError::ProjectionInvalid {
        message: format!("more {what} than the usage owner index can count"),
    }
}

fn session_started(conn: &Connection, session_id: &str) -> Result<Option<i64>, StoreError> {
    Ok(conn
        .prepare_cached("SELECT started_at FROM session WHERE id = ?1")?
        .query_row(params![session_id], |row| row.get::<_, i64>(0))
        .optional()?)
}

#[cfg(test)]
mod tests;
