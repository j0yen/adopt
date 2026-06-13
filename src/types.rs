//! Shared types for artifact scan results.

use serde::{Deserialize, Serialize};

/// The adoption verdict for a single artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    /// The binary is not present on PATH, ~/.local/bin, or ~/.cargo/bin.
    NotInstalled,
    /// The binary is installed but its mtime predates the repo's newest src/ commit.
    InstalledStale,
    /// The binary is installed and up to date with the src/ HEAD.
    InstalledCurrent,
    /// The repo declares no executable (library-only).
    NotABin,
}

impl Verdict {
    /// Returns true if the verdict requires user action to adopt.
    #[must_use]
    pub const fn is_actionable(&self) -> bool {
        matches!(self, Self::NotInstalled | Self::InstalledStale)
    }
}

/// The basis on which a freshness verdict was derived.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FreshnessBasis {
    /// Verdict came from comparing the install marker fingerprint to the repo's
    /// current committed-HEAD fingerprint. This is the authoritative signal.
    Lineage,
    /// No install marker was present; the verdict fell back to comparing the
    /// binary's mtime against the newest src/ commit timestamp (heuristic).
    ClockFallback,
}

/// A scanned artifact and its adoption verdict.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactResult {
    /// The wintermute repo path (absolute).
    pub repo: String,
    /// The binary name declared in Cargo.toml.
    pub bin: String,
    /// Adoption verdict.
    pub verdict: Verdict,
    /// Installed path if found (e.g. `~/.local/bin/foo`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_path: Option<String>,
    /// True if the artifact backs a systemd-user daemon unit.
    pub is_daemon: bool,
    /// Unix timestamp of the newest src/ commit (seconds since epoch), if determinable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_commit_ts: Option<i64>,
    /// Unix timestamp of the installed binary's mtime, if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_ts: Option<i64>,
    /// Copy-pasteable command to adopt this artifact. Empty for installed-current/not-a-bin.
    pub fix_cmd: String,
    /// Human-readable age relative to HEAD source (e.g. "9 days stale").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_vs_head: Option<String>,
    /// How the freshness verdict was derived.
    pub freshness_basis: FreshnessBasis,
}
