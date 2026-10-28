# aws-cost-report

A small Rust CLI that shows the AWS bill for the current month, a range of recent
complete months, or the last N months including the current one, for the account
behind a named profile and, when visible, its AWS Organization. It prints the
organization-wide total, a per-account breakdown, and service costs for every
visible account. It uses the official AWS SDK for Rust and the Cost Explorer
`GetCostAndUsage` API directly — no AWS CLI calls.

## What it does

- Accepts a named AWS profile with `-p` / `--profile`, including SSO profiles;
  defaults to `billing`.
- Reports the current month to date by default, the previous month with `-1`,
  the previous 3 complete months with `-3`, or the previous 6 with `-6`. These
  exclude the current month.
- Reports the last N months including the current one with `-m<NUMBER>`, where
  N is 1–12.
- The period options are mutually exclusive.
- Queries Cost Explorer for the selected period, grouped by `LINKED_ACCOUNT` and
  `SERVICE`.
- Looks up account names with AWS Organizations `ListAccounts` and shows each
  name next to its account number. If the lookup is unavailable, it falls back
  to account numbers only.
- Prints the organization-wide total, a service breakdown for every visible
  account, and a summary cost table by account, all sorted by cost. This mirrors
  the management account's billing console instead of reporting only the payer
  account's own usage.
- Omits accounts and services whose cost rounds to `$0.00`.
- Handles Cost Explorer pagination and validates API amounts.
- Forces the Cost Explorer region to `us-east-1`, where the global API lives.
- Exports the service cost table to a CSV file with `--csv FILE`.

```text
$ aws-cost-report -p management
Organizational Cost  $6,831.55
Period               October 2026 (month to date)

┌───────────────────────────┬────────────────────────────────────────┬───────────┐
│ ACCOUNT                   │ SERVICE                                │      COST │
├───────────────────────────┼────────────────────────────────────────┼───────────┤
│ Production (111122223333) │ Amazon Elastic Compute Cloud - Compute │ $2,500.00 │
│                           │ Amazon Relational Database Service     │   $700.00 │
│                           │ Amazon Simple Storage Service          │   $202.18 │
├───────────────────────────┼────────────────────────────────────────┼───────────┤
│                           │ ACCOUNT TOTAL                          │ $3,402.18 │
├───────────────────────────┼────────────────────────────────────────┼───────────┤
│ Shared (444455556666)     │ Amazon Elastic Compute Cloud - Compute │ $1,500.00 │
│                           │ Amazon Elastic Block Store             │   $645.00 │
├───────────────────────────┼────────────────────────────────────────┼───────────┤
│                           │ ACCOUNT TOTAL                          │ $2,145.00 │
├───────────────────────────┼────────────────────────────────────────┼───────────┤
│ Staging (123456789012)    │ Amazon Elastic Compute Cloud - Compute │   $812.44 │
│                           │ Amazon Relational Database Service     │   $221.10 │
│                           │ Amazon Simple Storage Service          │    $96.83 │
│                           │ Amazon CloudWatch                      │    $54.00 │
│                           │ Amazon Elastic Container Service       │    $41.20 │
│                           │ AWS Key Management Service             │    $22.40 │
│                           │ Amazon Route 53                        │    $16.40 │
│                           │ Amazon Virtual Private Cloud           │    $12.00 │
│                           │ Amazon DynamoDB                        │     $8.00 │
├───────────────────────────┼────────────────────────────────────────┼───────────┤
│                           │ ACCOUNT TOTAL                          │ $1,284.37 │
└───────────────────────────┴────────────────────────────────────────┴───────────┘

Cost by Account — October 2026 (month to date)
┌────────────┬──────────────┬───────────┐
│ ACCOUNT    │ ACCOUNT ID   │      COST │
├────────────┼──────────────┼───────────┤
│ Production │ 111122223333 │ $3,402.18 │
│ Shared     │ 444455556666 │ $2,145.00 │
│ Staging    │ 123456789012 │ $1,284.37 │
├────────────┼──────────────┼───────────┤
│ TOTAL      │              │ $6,831.55 │
└────────────┴──────────────┴───────────┘

Source: AWS Cost Explorer (UnblendedCost, estimated). Data can lag by up to 24 hours.
```

Each account is grouped with its services and an account subtotal, followed by a
summary table with every account's total. Zero-cost rows are omitted, and member
credentials only expose that member's own account.

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

# Current month to date with the default billing profile
./target/release/aws-cost-report

# Previous month
./target/release/aws-cost-report -1

# Last 3 complete months before the current one
./target/release/aws-cost-report -3

# Last 6 complete months before the current one
./target/release/aws-cost-report -6

# Last 3 months including the current one
./target/release/aws-cost-report -m3

# Select a different profile
./target/release/aws-cost-report -p my-sso-profile -3

# Also export the service cost table to a CSV file
./target/release/aws-cost-report -3 --csv costs.csv
```

Or with Cargo:

```bash
cargo run --release --
cargo run --release -- -p my-sso-profile
```

`-1`, `-3`, and `-6` select complete calendar months; the current partial month
is not included. `-m<NUMBER>` includes the current partial month, so `-m1` is
the same as the default. For example, in October 2026, `-3` reports July through
September 2026, while `-m3` reports August through October 2026. The `Period`
line shows the selected range. Only one period option may be used at a time.

## CSV export

Pass `--csv FILE` (or `-c FILE`) to write the service cost table to a CSV file.
The formatted tables are still printed, and a confirmation is written to stderr:

```bash
./target/release/aws-cost-report -3 --csv july-september-2026.csv
```

The file has one row per account and service, with the columns `period`,
`account_name`, `account_id`, `service`, `cost`, and `unit`:

```csv
period,account_name,account_id,service,cost,unit
July – September 2026,Production,111122223333,Amazon Elastic Compute Cloud - Compute,7500.00,USD
July – September 2026,Production,111122223333,Amazon Simple Storage Service,606.54,USD
July – September 2026,Shared,444455556666,Amazon Elastic Compute Cloud - Compute,4500.00,USD
```

Summing `cost` gives the organization total, and grouping by `account_id` gives
the cost-by-account table. Account names that are unavailable are left blank.
Fields containing commas, quotes, or line breaks are quoted per RFC 4180. If no
costs are recorded, the file contains only the header row.

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
`aws-cost-report`. If the session has expired, the SDK returns a credentials error;
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
