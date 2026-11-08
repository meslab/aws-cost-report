# aws-cost-report

## Build & Run
- Rust 1.94.1+ required (from Cargo.toml)
- Build: `cargo build --release`
- Run from ./target/release/aws-cost-report

## AWS Setup  
- Requires SSO profile or IAM credentials with ce:GetCostAndUsage permissions 
- Defaults to "billing" profile, override with `-p <profile>`
- Organization account names require organizations:ListAccounts permission
- Credentials can be via ~/.aws/config profiles (SSO supported)

## Source Structure
- `src/main.rs` - Entry point  
- `src/lib.rs` - Library entrypoint with ReportRequest and fetch functions
- `src/cli.rs` - Command line argument parsing 
- Uses aws-config, aws-sdk-costexplorer, aws-sdk-organizations crates

## Constraints
- Git commit messages must not exceed 72 characters