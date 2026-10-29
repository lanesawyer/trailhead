//! In-memory fakes for the factory's seams, so the whole lifecycle runs in CI
//! with no network or spend.

mod codehost;
mod compute;
mod daemon;
mod harness;
mod provider;
mod tracker;

pub use codehost::{FakeCodeHost, FakePr};
pub use compute::FakeCompute;
pub use daemon::{Connect, FAKE_BASE, FAKE_BASE_MERGE, FakeWorkspace, LocalDaemon};
pub use harness::{Script, ScriptedHarness};
pub use provider::FakeProvider;
pub use tracker::{FakeComment, FakeTracker};
