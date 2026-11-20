//! The lifecycle on fakes (`docs/factory/mvp.md`): spec to reviewed PR, budget breaches, failed checks, reconcile
//! after a crash, the review and the merger. One module per part of the control plane.

mod common;
mod grading;
mod lifecycle;
#[path = "../support/logs.rs"]
mod logs;
mod merger;
mod pool;
mod reconcile;
mod review;
mod run_loop;
mod targets;
