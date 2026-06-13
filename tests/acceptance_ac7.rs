//! AC7 (MUST): `adopt scan --format json | head -1` does not panic with SIGPIPE.

use std::process::{Command, Stdio};
use std::io::Read;

fn adopt_bin() -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("target/debug/adopt");
    p
}

#[test]
fn sigpipe_does_not_panic() {
    // Spawn adopt and pipe into `head -1`. When head closes its stdin after
    // the first line, adopt receives SIGPIPE on its next write. With
    // sigpipe::reset() this should be a clean process exit, not a panic.
    let mut adopt = Command::new(adopt_bin())
        .args(["scan", "--format", "json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn adopt");

    // Read just a few bytes from stdout then drop the reader (simulates `head`).
    {
        let mut stdout = adopt.stdout.take().expect("stdout");
        let mut buf = [0u8; 32];
        let _ = stdout.read(&mut buf); // read a little, then drop
        // stdout dropped here → pipe closed → adopt gets SIGPIPE on next write
    }

    let status = adopt.wait().expect("wait");

    let mut stderr_out = String::new();
    if let Some(mut se) = adopt.stderr.take() {
        let _ = se.read_to_string(&mut stderr_out);
    }

    // With sigpipe::reset() the process exits cleanly (signal or 0).
    // The critical assertion: no "BrokenPipe" / "panicked" in stderr.
    assert!(
        !stderr_out.contains("panicked") && !stderr_out.contains("BrokenPipe"),
        "SIGPIPE caused a panic:\nstderr: {stderr_out}\nexit: {status}"
    );
}
