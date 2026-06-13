//! `adopt` — detect shipped wintermute artifacts that never entered the live system.

use std::process::ExitCode;

mod cli;

fn main() -> ExitCode {
    // SIGPIPE reset: must be first line so `adopt scan | head` never panics.
    sigpipe::reset();

    match cli::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let msg = e.to_string();
            #[allow(clippy::print_stderr)]
            {
                eprintln!("adopt: {msg}");
            }
            ExitCode::FAILURE
        }
    }
}
