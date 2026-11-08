//! Command-line interface definitions.

use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use clap::{ArgGroup, Parser};

use crate::ReportRequest;
use crate::billing::{BillingPeriodError, Metric, PeriodSelection, billing_period};

/// The default SSO profile used when `--profile` is omitted.
pub const DEFAULT_PROFILE: &str = "billing";

/// The flag value that writes CSV output to standard output.
pub const STDOUT_PATH: &str = "-";

/// Show AWS costs by account and service for the current or recent months.
#[derive(Debug, Parser)]
#[command(name = "aws-cost-report", version, about)]
#[command(group(
    ArgGroup::new("period")
        .args([
            "previous_month",
            "last_three_months",
            "last_six_months",
            "months",
        ])
        .multiple(false)
))]
pub struct Cli {
    /// Named AWS profile to load credentials from, such as an SSO profile.
    ///
    /// Defaults to the "billing" SSO profile.
    #[arg(
        short = 'p',
        long = "profile",
        value_name = "PROFILE",
        default_value = DEFAULT_PROFILE
    )]
    profile: String,

    /// Export the service cost table to a CSV file, or to stdout with "-".
    #[arg(short = 'c', long = "csv", value_name = "FILE")]
    csv: Option<PathBuf>,

    /// Cost metric to report: `unblended` (before credits) or
    /// `net-unblended` (after credits).
    #[arg(
        long = "metric",
        value_name = "METRIC",
        default_value = "unblended",
        value_parser = parse_metric
    )]
    metric: Metric,

    /// Suppress the formatted tables and status messages.
    #[arg(short = 'q', long = "quiet")]
    quiet: bool,

    /// Skip the AWS Organizations lookup and show account numbers only.
    #[arg(long = "no-account-names")]
    no_account_names: bool,

    /// Report the previous calendar month.
    #[arg(short = '1', long = "previous-month")]
    previous_month: bool,

    /// Report the last 3 months before the current one.
    #[arg(short = '3', long = "last-3-months")]
    last_three_months: bool,

    /// Report the last 6 months before the current one.
    #[arg(short = '6', long = "last-6-months")]
    last_six_months: bool,

    /// Report the last NUMBER months, including the current one.
    #[arg(
        short = 'm',
        long = "months",
        value_name = "NUMBER",
        value_parser = parse_month_count
    )]
    months: Option<u32>,
}

impl Cli {
    /// The AWS profile to load credentials from.
    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// The optional path for the CSV export. [`STDOUT_PATH`] means stdout.
    pub fn csv(&self) -> Option<&Path> {
        self.csv.as_deref()
    }

    /// Whether CSV output should be written to standard output.
    pub fn csv_to_stdout(&self) -> bool {
        self.csv()
            .is_some_and(|path| path == Path::new(STDOUT_PATH))
    }

    /// The Cost Explorer metric to report.
    pub fn metric(&self) -> Metric {
        self.metric
    }

    /// Whether formatted tables and status messages should be suppressed.
    pub fn quiet(&self) -> bool {
        self.quiet
    }

    /// Whether the AWS Organizations account-name lookup should be skipped.
    pub fn no_account_names(&self) -> bool {
        self.no_account_names
    }

    /// The period selected by the period flags.
    pub fn period_selection(&self) -> PeriodSelection {
        if self.previous_month {
            PeriodSelection::CompleteMonthsBefore(1)
        } else if self.last_three_months {
            PeriodSelection::CompleteMonthsBefore(3)
        } else if self.last_six_months {
            PeriodSelection::CompleteMonthsBefore(6)
        } else if let Some(months) = self.months {
            PeriodSelection::MonthsIncludingCurrent(months)
        } else {
            PeriodSelection::MonthToDate
        }
    }

    /// Build a CLI-independent report request for `today`.
    pub fn report_request(&self, today: NaiveDate) -> Result<ReportRequest, BillingPeriodError> {
        Ok(ReportRequest {
            profile: self.profile.clone(),
            period: billing_period(today, self.period_selection())?,
            metric: self.metric,
            include_account_names: !self.no_account_names,
        })
    }
}

fn parse_metric(value: &str) -> Result<Metric, String> {
    value.parse()
}

fn parse_month_count(value: &str) -> Result<u32, String> {
    let months = value
        .parse::<u32>()
        .map_err(|_| "must be a whole number of months from 1 to 12".to_owned())?;

    if (1..=12).contains(&months) {
        Ok(months)
    } else {
        Err("must be from 1 to 12 months".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("valid test date")
    }

    #[test]
    fn parses_profile_flags() {
        let cli = Cli::try_parse_from(["aws-cost-report", "--profile", "billing-sso"])
            .expect("parse long flag");
        assert_eq!(cli.profile, "billing-sso");

        let cli = Cli::try_parse_from(["aws-cost-report", "-p", "billing-sso"])
            .expect("parse short flag");
        assert_eq!(cli.profile, "billing-sso");
    }

    #[test]
    fn uses_billing_profile_and_current_month_by_default() {
        let cli = Cli::try_parse_from(["aws-cost-report"]).expect("parse defaults");
        assert_eq!(cli.profile, DEFAULT_PROFILE);
        assert_eq!(cli.metric(), Metric::Unblended);
        assert!(!cli.quiet());
        assert!(!cli.no_account_names());
        assert!(matches!(
            cli.period_selection(),
            PeriodSelection::MonthToDate
        ));
    }

    #[test]
    fn parses_metric_flags() {
        let cli = Cli::try_parse_from(["aws-cost-report", "--metric", "net-unblended"])
            .expect("parse metric");
        assert_eq!(cli.metric(), Metric::NetUnblended);

        assert!(Cli::try_parse_from(["aws-cost-report", "--metric", "gross"]).is_err());
    }

    #[test]
    fn parses_script_friendly_flags() {
        let cli = Cli::try_parse_from([
            "aws-cost-report",
            "--quiet",
            "--no-account-names",
            "--csv",
            "-",
        ])
        .expect("parse script flags");

        assert!(cli.quiet());
        assert!(cli.no_account_names());
        assert!(cli.csv_to_stdout());
    }

    #[test]
    fn parses_period_flags() {
        for (flag, months) in [("-1", 1), ("-3", 3), ("-6", 6)] {
            let cli = Cli::try_parse_from(["aws-cost-report", flag]).expect("parse period flag");
            assert!(matches!(
                cli.period_selection(),
                PeriodSelection::CompleteMonthsBefore(value) if value == months
            ));
        }

        for (flag, months) in [
            ("--previous-month", 1),
            ("--last-3-months", 3),
            ("--last-6-months", 6),
        ] {
            let cli = Cli::try_parse_from(["aws-cost-report", flag]).expect("parse period flag");
            assert!(matches!(
                cli.period_selection(),
                PeriodSelection::CompleteMonthsBefore(value) if value == months
            ));
        }
    }

    #[test]
    fn parses_months_including_the_current_month() {
        for (args, months) in [
            (vec!["aws-cost-report", "-m3"], 3),
            (vec!["aws-cost-report", "-m", "6"], 6),
            (vec!["aws-cost-report", "--months", "12"], 12),
        ] {
            let cli = Cli::try_parse_from(args).expect("parse months flag");
            assert!(matches!(
                cli.period_selection(),
                PeriodSelection::MonthsIncludingCurrent(value) if value == months
            ));
        }
    }

    #[test]
    fn months_must_be_between_one_and_twelve() {
        assert!(Cli::try_parse_from(["aws-cost-report", "-m0"]).is_err());
        assert!(Cli::try_parse_from(["aws-cost-report", "-m13"]).is_err());
        assert!(Cli::try_parse_from(["aws-cost-report", "-mabc"]).is_err());
    }

    #[test]
    fn period_flags_are_mutually_exclusive() {
        assert!(Cli::try_parse_from(["aws-cost-report", "-1", "-3"]).is_err());
        assert!(Cli::try_parse_from(["aws-cost-report", "-3", "--last-6-months"]).is_err());
        assert!(Cli::try_parse_from(["aws-cost-report", "-m3", "-1"]).is_err());
    }

    #[test]
    fn account_is_not_a_cli_argument() {
        assert!(Cli::try_parse_from(["aws-cost-report", "-a", "123456789012"]).is_err());
    }

    #[test]
    fn builds_a_report_request() {
        let cli = Cli::try_parse_from([
            "aws-cost-report",
            "--metric",
            "net-unblended",
            "--no-account-names",
            "-1",
        ])
        .expect("parse report flags");
        let request = cli
            .report_request(date(2026, 10, 28))
            .expect("valid report request");

        assert_eq!(request.profile, DEFAULT_PROFILE);
        assert_eq!(request.period.start(), date(2026, 9, 1));
        assert_eq!(request.period.end(), date(2026, 10, 1));
        assert_eq!(request.metric, Metric::NetUnblended);
        assert!(!request.include_account_names);
    }
}
