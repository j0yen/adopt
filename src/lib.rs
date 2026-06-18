//! `adopt` library crate — re-exports public types for integration tests and proptest.

pub mod types;
pub mod scan;
pub mod apply;
pub mod marker;
pub mod reconcile;
pub mod report;
pub mod doctor;
pub mod verify;
pub mod converge;

/// Process-wide environment lock for tests that mutate env vars.
///
/// All test modules that set/remove `XDG_STATE_HOME`, `HOME`, `WM_WINTERMUTE_DIR`,
/// or `PATH` must hold this lock for the duration of the mutation.  Using a single
/// shared lock ensures no two env-mutating tests run concurrently in the same process,
/// regardless of which module they live in.
#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
