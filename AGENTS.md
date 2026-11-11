# aws-cost-report

Rust CLI that reports AWS costs by account and service via Cost Explorer.

## Build, run, verify
- Rust 1.94.1+ required (`rust-version` in Cargo.toml; needed by the AWS SDK).
- Build: `cargo build --release` → run from `./target/release/aws-cost-report`.
- Verify before finishing, in order: `cargo fmt --check`, then `cargo clippy --all-targets --all-features -- -D warnings`, then `cargo test`. All must pass.
- Tests mock the Cost Explorer HTTP endpoint — no AWS credentials or network needed.

## Run
- Defaults to the AWS `billing` profile; pick another with `-p <profile>`.
- Period flags are mutually exclusive: `-1`/`-3`/`-6` for complete past months (excludes the current month), `-m<N>` (N=1–12) for the last N months including the current partial one.
- Export the service table: `--csv FILE`.

## Credentials (gotchas)
- Needs `ce:GetCostAndUsage`; `organizations:ListAccounts` is optional and only adds account names (falls back to account numbers).
- SSO profiles: run `aws sso login --profile <name>` first — the SDK uses the cached token and errors if it has expired.
- Cost Explorer is global; the client forces region `us-east-1` regardless of the profile's region.

## Structure
- `src/main.rs` — CLI entry, reports errors.
- `src/lib.rs` — public `run` entry point and shared constants.
- `src/cli.rs` — argument parsing and period selection.
- `src/billing.rs` — billing periods and cost summary model.
- `src/aws.rs` — Cost Explorer and Organizations API calls.
- `src/report.rs` — formatted tables and CSV export.

## Constraints
- Commit messages must not exceed 72 characters (Conventional Commits).
- Never hand-edit `Cargo.lock`; use `cargo update` to change it.
