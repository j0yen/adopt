# adopt

Detect shipped wintermute artifacts that never entered the live system.

## The problem

`/build` ships a repo by build → commit → push. But **adoption** — installing
the binary so it lands on `$PATH` and becomes invokable — is a separate step
that frequently never runs. The result: tools that were fully built, tested,
and committed sit unused for days.

`binstale` can tell you whether a *running* daemon is executing stale bytes.
It structurally cannot flag a CLI that was *never installed and therefore
never runs*. `adopt scan` fills that gap.

## Usage

```
adopt scan                        # table of unadopted / stale artifacts
adopt scan --format json          # machine-readable JSON array
adopt scan --all                  # include installed-current and library-only
adopt scan --match '^wm-'         # restrict to bins matching regex
```

## Verdicts

| Verdict | Meaning |
|---|---|
| `not-installed` | Binary absent from PATH, ~/.local/bin, ~/.cargo/bin |
| `installed-stale` | Binary installed but older than repo's newest src/ commit |
| `installed-current` | Binary installed and up to date |
| `not-a-bin` | Library-only repo (shown only with `--all`) |

## Fix commands

Every actionable verdict includes a copy-pasteable `fix_cmd`:

```
cargo install --path ~/wintermute/<repo> --root ~/.local
```

For daemon-backed artifacts (`is_daemon: true`), the fix is:

```
rollout install ~/wintermute/<repo>
```

## Acceptance criteria

1. `--format json` emits array with `repo`, `bin`, `verdict`, `is_daemon`, `fix_cmd` keys
2. Not-installed binary → verdict `not-installed` + fix_cmd with `--root ~/.local`
3. Binary newer than src/ commit → `installed-current`
4. Binary older than src/ commit → `installed-stale`
5. Library-only repo → `not-a-bin`, hidden from default output
6. Malformed Cargo.toml / missing dir → skipped, exit 0
7. `adopt scan --format json | head -1` does not panic (SIGPIPE reset)
8. `--match <regex>` restricts output to matching bin names
9. Daemon artifact (systemd ExecStart) → `is_daemon: true`
10. `adopt --version` and `adopt --help` exit 0

## Install

```
cargo install --path . --root ~/.local
```

## License

MIT OR Apache-2.0
