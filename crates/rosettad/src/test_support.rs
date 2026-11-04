//! The integration tests' generated fixtures (`tests/support/`), shared with
//! the in-crate tests that drive the real writer and the real `serve`: the
//! same R1 brief §10 shapes, generated into each test's own temporary home.
//! Never a live home directory, never committed raw data.

#[allow(dead_code)]
#[path = "../tests/support/crossing.rs"]
pub(crate) mod crossing;
#[allow(dead_code)]
#[path = "../tests/support/fixtures.rs"]
pub(crate) mod fixtures;
#[allow(dead_code)]
#[path = "../tests/support/harness.rs"]
pub(crate) mod harness;
#[allow(dead_code)]
#[path = "../tests/support/projection_dump.rs"]
pub(crate) mod projection_dump;
