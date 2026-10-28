# aws-monitor

A small Rust CLI that shows the current month-to-date bill for the AWS account
behind a named profile and, when visible, its AWS Organization. It prints the
organization-wide total, a per-account breakdown, and service costs for every
visible account. It uses the official AWS SDK for Rust and the Cost Explorer
`GetCostAndUsage` API directly — no AWS CLI calls.

## What it does

- Accepts a named AWS profile with `-p` / `--profile`, including SSO profiles;
  defaults to `billing`.
- Queries Cost Explorer for the current calendar month, grouped by
  `LINKED_ACCOUNT` and `SERVICE`.
- Looks up account names with AWS Organizations `ListAccounts` and shows each
  name next to its account number. If the lookup is unavailable, it falls back
  to account numbers only.
- Prints the organization-wide total, a per-account cost table, and a service
  cost table for every visible account, all sorted by cost. This mirrors the
  management account's billing console instead of reporting only the payer
  account's own usage.
- Omits accounts and services whose cost rounds to `$0.00`.
- Handles Cost Explorer pagination and validates API amounts.
- Forces the Cost Explorer region to `us-east-1`, where the global API lives.

```text
$ aws-monitor -p management
Organizational Cost $8,921.55
Period              October 2026 (month to date)

Cost by Account
ACCOUNT NAME  ACCOUNT            COST
------------  ------------  ---------
Production    111122223333  $3,402.18
Shared        444455556666  $2,145.00
Staging       123456789012  $1,284.37
Analytics     777788889999  $1,090.37
Sandbox       999900001111    $999.63

Service Costs by Account
Staging (123456789012)
SERVICE                                      COST
-------------------------------------------  ---------
Amazon Elastic Compute Cloud - Compute      $812.44
Amazon Relational Database Service          $221.10
Amazon Simple Storage Service               $96.83
Amazon CloudWatch                           $54.00
Amazon Elastic Container Service            $41.20
AWS Key Management Service                  $22.40
Amazon Route 53                             $16.40
Amazon Virtual Private Cloud                $12.00
Amazon DynamoDB                             $8.00

Source: AWS Cost Explorer (UnblendedCost, estimated). Data can lag by up to 24 hours.
```

Each visible account gets its own service table. Zero-cost rows are omitted,
and member credentials only expose that member's own account.

## Requirements

- Rust **1.94.1 or newer** (required by the current AWS SDK crates).
- A configured AWS profile named `billing` by default; override it with `-p`.
- The `ce:GetCostAndUsage` permission.
- Cost Explorer enabled for the account.
- The `organizations:ListAccounts` permission to show account names. Without
  it, the app prints a warning and falls back to account numbers.
- Management-account credentials if you want organization-wide totals; member
  credentials only expose that member's own costs.

## Build and run

```bash
cargo build --release

# Use the default billing profile
./target/release/aws-monitor

# Select a different profile
./target/release/aws-monitor -p my-sso-profile
```

Or with Cargo:

```bash
cargo run --release --
cargo run --release -- -p my-sso-profile
```

## Credentials

The app loads the profile selected by `-p` / `--profile`. When the flag is
omitted, it uses the `billing` profile:

```bash
# Use the default billing profile
cargo run --release --

# Select a different profile
cargo run --release -- -p billing-sso
```

The app itself never invokes the AWS CLI. For an SSO profile, the SDK uses the
cached SSO access token, so establish the session first with
`aws sso login --profile <profile>` (or another SSO client), then run
`aws-monitor`. If the session has expired, the SDK returns a credentials error;
re-run the login command.

## IAM permissions

A minimal policy for querying costs and resolving account names:

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": "ce:GetCostAndUsage",
      "Resource": "*"
    },
    {
      "Effect": "Allow",
      "Action": "organizations:ListAccounts",
      "Resource": "*"
    }
  ]
}
```

`organizations:ListAccounts` is optional; without it, the app still reports
costs but shows account numbers instead of names.

## Linked accounts

Cost Explorer exposes linked-account costs to the **management account** of an
AWS Organization. With management-account credentials the app reports:

- **Organizational Cost** — the consolidated total across every linked account
  the credentials can see.
- **Cost by Account** — each visible account's name, number, and own usage,
  largest first.
- **Service Costs by Account** — each visible account's service breakdown.

The organizational total is the number shown on the management account's
billing dashboard, so it can be much larger than the payer account's own usage.
No account flag is needed: the account table and service breakdowns cover every
account the credentials can see. Member credentials only expose that member's
own costs, so the complete organization view is available only from the
management account.

## Cost and data notes

- The app requests the `UnblendedCost` metric. This is usage cost before credits
  and refunds, so it can differ from the final invoice (which also includes
  taxes and other adjustments). To show net cost after credits/refunds, change
  `METRIC` in `src/main.rs` to `NetUnblendedCost`.
- Cost Explorer data is updated at least once every 24 hours, so the current
  day's charges may be incomplete.
- The Cost Explorer API charges **$0.01 per request**. This app makes one
  `GetCostAndUsage` request per run (plus pagination requests if the result set
  is large).
- Cost Explorer's API endpoint is `us-east-1` for commercial AWS; the client
  sets that region regardless of the profile's configured region. Credentials
  are still loaded normally.

## Development

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

The test suite includes a mocked Cost Explorer HTTP endpoint, so it does not
make AWS calls. The CLI uses [`clap`](https://docs.rs/clap) derive parsing, and
the AWS calls use `aws-sdk-costexplorer`, `aws-sdk-organizations`, and
`aws-config`.
