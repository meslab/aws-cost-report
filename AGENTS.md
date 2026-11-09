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
- Never edit `Cargo.lock` use `cargo update` command to update its content

## Key Dependencies
- aws-config: 1.13.0 (with features: behavior-version-latest, credentials-process, legacy-client, rt-tokio, sso)
- aws-sdk-costexplorer: 1.138.0 (with features: rt-tokio, rustls)
- aws-sdk-organizations: 1.137.0 (with features: rt-tokio, rustls)
- clap: 4.6.7 (with features: derive)
