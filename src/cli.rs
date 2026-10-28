//! Command-line interface definitions.

use std::path::{Path, PathBuf};

use clap::{ArgGroup, Parser};

use crate::billing::PeriodSelection;

/// The default SSO profile used when `--profile` is omitted.
pub const DEFAULT_PROFILE: &str = "billing";

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

    /// Export the service cost table to a CSV file.
    #[arg(short = 'c', long = "csv", value_name = "FILE")]
    csv: Option<PathBuf>,

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

    /// The optional path for the CSV export.
    pub fn csv(&self) -> Option<&Path> {
        self.csv.as_deref()
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
    use super::*;

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
        assert!(matches!(
            cli.period_selection(),
            PeriodSelection::MonthToDate
        ));
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
}
