//! Show AWS costs by account and service for the current or recent months.
//!
//! The [`run`] function fetches a cost report from AWS Cost Explorer and writes
//! the human-readable tables, optionally exporting CSV. Library consumers can
//! build a [`ReportRequest`] and call [`fetch_summary`] or [`fetch_report`] to
//! obtain the data without producing output.
//!
//! The binary in `src/main.rs` only parses the command line and reports errors.

#![forbid(unsafe_code)]

pub mod billing;
pub mod cli;
pub mod report;

mod aws;

use std::collections::HashMap;
use std::io::{self, Write};

use anyhow::Result;
use aws_sdk_costexplorer::Client as CostExplorerClient;
use aws_sdk_organizations::Client as OrganizationsClient;
use chrono::Utc;

pub use billing::{BillingPeriod, BillingPeriodError, BillingSummary, Metric, PeriodSelection};
pub use cli::Cli;

/// A CLI-independent request for a cost report.
#[derive(Debug, Clone)]
pub struct ReportRequest {
    /// Named AWS profile to load credentials from.
    pub profile: String,
    /// The validated period to report.
    pub period: BillingPeriod,
    /// The Cost Explorer metric to request.
    pub metric: Metric,
    /// Whether to resolve account names with AWS Organizations.
    pub include_account_names: bool,
}

impl ReportRequest {
    /// Build a request with the default metric and account-name lookup.
    pub fn new(profile: impl Into<String>, period: BillingPeriod) -> Self {
        Self {
            profile: profile.into(),
            period,
            metric: Metric::default(),
            include_account_names: true,
        }
    }

    /// Select a different Cost Explorer metric.
    #[must_use]
    pub fn with_metric(mut self, metric: Metric) -> Self {
        self.metric = metric;
        self
    }

    /// Enable or disable the AWS Organizations account-name lookup.
    #[must_use]
    pub fn with_account_names(mut self, include: bool) -> Self {
        self.include_account_names = include;
        self
    }
}

/// A fetched report plus any non-fatal warnings.
#[derive(Debug)]
pub struct FetchedReport {
    /// The aggregated cost data.
    pub summary: BillingSummary,
    /// Non-fatal problems, such as a failed account-name lookup.
    pub warnings: Vec<String>,
}

/// Fetch a cost report, including non-fatal warnings.
pub async fn fetch_report(request: &ReportRequest) -> Result<FetchedReport> {
    let config = aws::load_config(&request.profile).await;
    let cost_client = CostExplorerClient::new(&config);

    let (data, account_names, warnings) = if request.include_account_names {
        let organizations = OrganizationsClient::new(&config);
        // The cost query and the account-name lookup are independent, so start
        // both before awaiting either. Pagination inside each call is inherently
        // sequential because every page supplies the next page's token.
        let (cost_result, names_result) = tokio::join!(
            aws::fetch_costs(&cost_client, request.period, request.metric),
            aws::fetch_account_names(&organizations),
        );

        match names_result {
            Ok(names) => (cost_result?, names, Vec::new()),
            Err(error) => (
                cost_result?,
                HashMap::new(),
                vec![format!("could not load account names: {error:#}")],
            ),
        }
    } else {
        (
            aws::fetch_costs(&cost_client, request.period, request.metric).await?,
            HashMap::new(),
            Vec::new(),
        )
    };

    let summary = aws::assemble_summary(request.period, request.metric, data, &account_names);
    Ok(FetchedReport { summary, warnings })
}

/// Fetch a cost report and return only the summary.
///
/// Use [`fetch_report`] when the account-name lookup warning matters.
pub async fn fetch_summary(request: &ReportRequest) -> Result<BillingSummary> {
    Ok(fetch_report(request).await?.summary)
}

/// Fetch costs for the period selected by `cli` and write the report.
pub async fn run(cli: Cli) -> Result<()> {
    let request = cli.report_request(Utc::now().date_naive())?;
    let fetched = fetch_report(&request).await?;

    if !cli.quiet() {
        for warning in &fetched.warnings {
            let _ = writeln!(io::stderr(), "warning: {warning}");
        }
    }

    write_output(&cli, &fetched.summary)
}

fn write_output(cli: &Cli, summary: &BillingSummary) -> Result<()> {
    if let Some(path) = cli.csv() {
        if cli.csv_to_stdout() {
            let stdout = io::stdout();
            let mut writer = stdout.lock();
            report::write_csv(&mut writer, summary)?;
            writer.flush()?;
            return Ok(());
        }

        report::write_csv_file(path, summary)?;
        if !cli.quiet() {
            let _ = writeln!(io::stderr(), "wrote CSV export to {}", path.display());
        }
    }

    if !cli.quiet() {
        let stdout = io::stdout();
        let mut writer = stdout.lock();
        report::render_summary(&mut writer, summary)?;
        writer.flush()?;
    }

    Ok(())
}

/// Whether an error was caused by a closed output pipe, such as `| head`.
///
/// Callers can treat this as a successful early exit.
pub fn is_broken_pipe(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == io::ErrorKind::BrokenPipe)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_request_builders_set_options() {
        let period = BillingPeriod::new(
            chrono::NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
            chrono::NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(),
            true,
        )
        .expect("valid period");

        let request = ReportRequest::new("billing", period)
            .with_metric(Metric::NetUnblended)
            .with_account_names(false);

        assert_eq!(request.profile, "billing");
        assert_eq!(request.metric, Metric::NetUnblended);
        assert!(!request.include_account_names);
    }

    #[test]
    fn detects_broken_pipe_errors() {
        let error = anyhow::Error::new(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
            .context("write report");
        assert!(is_broken_pipe(&error));

        let error = anyhow::Error::new(io::Error::new(io::ErrorKind::PermissionDenied, "denied"))
            .context("write report");
        assert!(!is_broken_pipe(&error));
    }
}
