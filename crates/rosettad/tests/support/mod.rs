//! The harness shared by the streamed fresh-cold integration tests (R1 brief
//! §10): the logical projection dump, fresh serve's pipeline steps in
//! process, a harness copy of the R1 writer, and the generated fixtures.
//! Each test binary uses part of it.
#![allow(dead_code)]

pub mod crossing;
pub mod fixtures;
pub mod harness;
pub mod projection_dump;
pub mod writer;
