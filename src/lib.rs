//! Show AWS costs by account and service for the current or recent months.
//!
//! The [`run`] function fetches a cost report from AWS Cost Explorer and prints
//! the human-readable tables, optionally writing a CSV export. The binary in
//! `src/main.rs` only parses the command line and reports errors.

pub mod billing;
pub mod cli;
pub mod report;

mod aws;

use std::collections::HashMap;

use anyhow::Result;
use aws_sdk_costexplorer::Client as CostExplorerClient;
use aws_sdk_organizations::Client as OrganizationsClient;
use chrono::Utc;

pub use cli::Cli;

/// The metric requested from Cost Explorer and shown in the report.
pub(crate) const METRIC: &str = "UnblendedCost";

/// Fetch costs for the period selected by `cli` and print the report.
pub async fn run(cli: Cli) -> Result<()> {
    let config = aws::load_config(cli.profile()).await;
    let period = billing::billing_period(Utc::now().date_naive(), cli.period_selection());
    let account_names = match aws::fetch_account_names(&OrganizationsClient::new(&config)).await {
        Ok(names) => names,
        Err(error) => {
            eprintln!("warning: could not load account names: {error:#}");
            HashMap::new()
        }
    };

    let client = CostExplorerClient::new(&config);
    let summary = aws::fetch_billing(&client, period, &account_names).await?;

    if let Some(path) = cli.csv() {
        report::write_csv(path, &summary)?;
        eprintln!("wrote CSV export to {}", path.display());
    }

    report::print_summary(&summary);

    Ok(())
}
