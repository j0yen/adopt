# adopt

Find the wintermute tools that were built, tested, and committed — but never installed, so they never ran.

Shipping a repo and adopting it are different acts. `/build` ships: build, commit, push. Adoption is the separate step that installs the binary onto `$PATH` so something can actually invoke it — and it is the step that quietly never runs. The result is a tool that passed every gate and then sat unused for days, indistinguishable from one that was never written.

Nothing else catches this. `binstale` can tell you a *running* daemon is executing stale bytes, but a CLI that was never installed never runs, so there is no process to inspect and no stale bytes to find. The absence is the bug, and absence is exactly what a process-based check cannot see. `adopt` closes that gap: it compares what the repos shipped against what is installed, and names the difference.

## Install

```sh
cargo install --path . --root ~/.local
```

## Quickstart

```sh
adopt scan            # table of unadopted and stale artifacts
adopt apply           # dry-run: show what installing them would do
adopt apply --execute # actually install the unadopted non-daemon tools
```

`adopt scan` walks `~/wintermute` (override with `WM_WINTERMUTE_DIR`), checks each repo's binary against `~/.local/bin`, `~/.cargo/bin`, and `$PATH`, and prints a verdict per artifact:

| Verdict | Meaning |
|---|---|
| `not-installed` | The binary is on no install path. It shipped and never landed. |
| `installed-stale` | Installed, but behind the repo's current committed source. |
| `installed-current` | Installed and up to date. |
| `not-a-bin` | Library-only repo (shown only with `--all`). |

Each actionable verdict carries a copy-pasteable `fix_cmd` — `cargo install --force --path <repo> --root ~/.local` for a plain binary, or `rollout install <repo>` for a daemon-backed one (`is_daemon: true`). Pass `--format json` for the machine-readable array, `--all` to include current and library-only entries, `--match <regex>` to restrict by bin name.

## Freshness is lineage, not the clock

A binary installed five seconds before its own commit looks "older than source" by timestamp and gets falsely flagged stale. `adopt` does not trust the clock for that decision. `adopt apply` writes an `InstallMarker` recording the committed-HEAD fingerprint it installed from; `adopt scan` reads that marker back and calls a binary current only when its fingerprint equals the repo's current HEAD. Every artifact record reports a `freshness_basis` — `lineage` when a marker proved the verdict, `clock-fallback` when no marker existed and timestamps were the only evidence. The distinction matters: lineage is proof, the clock is a guess.

## The other subcommands

`scan` and `apply` are the loop most people run. The rest support a self-converging pipeline:

- `adopt report --run <id>` — file the unadopted findings to the docket ledger, separating genuinely-behind (lineage) installs from merely-unmarked ones so the count stays honest.
- `adopt converge [--last N] [--run <id>]` — show the behind-count trend across runs and alert when it fails to reach zero for `--stall-runs` runs (default 4).
- `adopt reconcile` — mint lineage markers for binaries that were installed before markers existed, without rebuilding them. Clears the legacy false-positive floor.
- `adopt verify` — classify not-current artifacts into named failure buckets.
- `adopt doctor` — detect and optionally clean junk binaries adopt created under a literal-tilde prefix.

Every mutating subcommand defaults to dry-run; `--execute` or the absence of `--dry-run` is what makes it act.

## Where it fits

`adopt` is the adoption check in the wintermute build pipeline, paired with `binstale` (stale running daemons) and `rollout` (daemon installs, which `adopt apply --with-daemons` delegates to). Between them they cover the three ways a shipped tool fails to be live: never installed, installed-stale, or running stale.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
