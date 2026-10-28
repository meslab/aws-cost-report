use std::collections::HashMap;
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, ensure};
use aws_config::{BehaviorVersion, Region, SdkConfig};
use aws_sdk_costexplorer::Client as CostExplorerClient;
use aws_sdk_costexplorer::types::{
    DateInterval, Granularity, GroupDefinition, GroupDefinitionType,
};
use aws_sdk_organizations::Client as OrganizationsClient;
use chrono::{Datelike, NaiveDate, Utc};
use clap::{ArgGroup, Parser};

/// Cost Explorer's API is hosted in us-east-1, regardless of where the
/// account's resources live.
const COST_EXPLORER_REGION: &str = "us-east-1";
const METRIC: &str = "UnblendedCost";
const DEFAULT_PROFILE: &str = "billing";

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
struct Cli {
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
    fn period_selection(&self) -> PeriodSelection {
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

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    let config = load_config(&cli.profile).await;
    let period = billing_period(Utc::now().date_naive(), cli.period_selection());
    let account_names = match fetch_account_names(&OrganizationsClient::new(&config)).await {
        Ok(names) => names,
        Err(error) => {
            eprintln!("warning: could not load account names: {error:#}");
            HashMap::new()
        }
    };

    let client = CostExplorerClient::new(&config);
    let summary = fetch_billing(&client, period, &account_names).await?;
    print_summary(&summary);

    Ok(())
}

async fn fetch_account_names(client: &OrganizationsClient) -> Result<HashMap<String, String>> {
    let mut names = HashMap::new();
    let mut next_page_token: Option<String> = None;

    loop {
        let mut request = client.list_accounts();

        if let Some(token) = next_page_token.take() {
            request = request.next_token(token);
        }

        let response = request.send().await.context(
            "AWS Organizations ListAccounts failed \
             (requires organizations:ListAccounts, usually from the management account)",
        )?;

        for account in response.accounts() {
            if let (Some(id), Some(name)) = (account.id(), account.name()) {
                names.insert(id.to_owned(), name.to_owned());
            }
        }

        next_page_token = response.next_token().map(str::to_owned);
        if next_page_token.is_none() {
            break;
        }
    }

    Ok(names)
}

async fn load_config(profile: &str) -> SdkConfig {
    aws_config::defaults(BehaviorVersion::latest())
        .region(Region::new(COST_EXPLORER_REGION))
        .profile_name(profile)
        .load()
        .await
}

async fn fetch_billing(
    client: &CostExplorerClient,
    period: BillingPeriod,
    account_names: &HashMap<String, String>,
) -> Result<BillingSummary> {
    let time_period = DateInterval::builder()
        .start(period.start.format("%Y-%m-%d").to_string())
        .end(period.end.format("%Y-%m-%d").to_string())
        .build()
        .context("failed to build the Cost Explorer time period")?;

    let mut account_service_costs: HashMap<String, HashMap<String, f64>> = HashMap::new();
    let mut unit: Option<String> = None;
    let mut estimated = false;
    let mut next_page_token: Option<String> = None;

    loop {
        let mut request = client
            .get_cost_and_usage()
            .time_period(time_period.clone())
            .granularity(Granularity::Monthly)
            .metrics(METRIC)
            .group_by(
                GroupDefinition::builder()
                    .r#type(GroupDefinitionType::Dimension)
                    .key("LINKED_ACCOUNT")
                    .build(),
            )
            .group_by(
                GroupDefinition::builder()
                    .r#type(GroupDefinitionType::Dimension)
                    .key("SERVICE")
                    .build(),
            );

        if let Some(token) = next_page_token.take() {
            request = request.next_page_token(token);
        }

        let response = request.send().await.context(
            "Cost Explorer GetCostAndUsage failed \
             (requires ce:GetCostAndUsage; organization-wide data requires the \
             organization's management account)",
        )?;

        for result in response.results_by_time() {
            estimated |= result.estimated();

            for group in result.groups() {
                let keys = group.keys();
                ensure!(
                    keys.len() == 2,
                    "Cost Explorer returned a group with {} keys; \
                     expected LINKED_ACCOUNT and SERVICE",
                    keys.len()
                );

                let linked_account = &keys[0];
                let service = keys[1].clone();

                let metric = group
                    .metrics()
                    .and_then(|metrics| metrics.get(METRIC))
                    .ok_or_else(|| {
                        anyhow!("Cost Explorer returned a group without a {METRIC} metric")
                    })?;

                if let Some(group_unit) = metric.unit() {
                    if let Some(existing_unit) = &unit {
                        ensure!(
                            existing_unit == group_unit,
                            "Cost Explorer returned mixed units: {existing_unit} and {group_unit}"
                        );
                    } else {
                        unit = Some(group_unit.to_owned());
                    }
                }

                let amount = metric
                    .amount()
                    .ok_or_else(|| {
                        anyhow!("Cost Explorer returned a {METRIC} metric without an amount")
                    })?
                    .parse::<f64>()
                    .with_context(|| {
                        format!(
                            "Cost Explorer returned an invalid {METRIC} amount for {service} \
                             in account {linked_account}"
                        )
                    })?;

                *account_service_costs
                    .entry(linked_account.clone())
                    .or_default()
                    .entry(service)
                    .or_insert(0.0) += amount;
            }
        }

        next_page_token = response.next_page_token().map(str::to_owned);
        if next_page_token.is_none() {
            break;
        }
    }

    let organization_total = account_service_costs
        .values()
        .flat_map(|services| services.values())
        .sum();

    let mut accounts: Vec<AccountCost> = account_service_costs
        .into_iter()
        .filter_map(|(account_id, service_costs)| {
            let amount: f64 = service_costs.values().sum();
            if !is_displayable_cost(amount) {
                return None;
            }

            let name = account_names.get(&account_id).cloned();
            let mut services: Vec<ServiceCost> = service_costs
                .into_iter()
                .filter(|(_, amount)| is_displayable_cost(*amount))
                .map(|(name, amount)| ServiceCost { name, amount })
                .collect();
            services.sort_by(|a, b| {
                b.amount
                    .total_cmp(&a.amount)
                    .then_with(|| a.name.cmp(&b.name))
            });

            Some(AccountCost {
                account_id,
                name,
                amount,
                services,
            })
        })
        .collect();
    accounts.sort_by(|a, b| {
        b.amount
            .total_cmp(&a.amount)
            .then_with(|| a.account_id.cmp(&b.account_id))
    });

    Ok(BillingSummary {
        period,
        unit: unit.unwrap_or_else(|| "USD".to_owned()),
        organization_total,
        accounts,
        estimated,
    })
}

/// Cost Explorer can return explicit zero-cost groups. Hide amounts that would
/// render as $0.00, such as a service with no usage this month.
fn is_displayable_cost(amount: f64) -> bool {
    amount.abs() >= 0.005
}

fn billing_period(today: NaiveDate, selection: PeriodSelection) -> BillingPeriod {
    let current_start = NaiveDate::from_ymd_opt(today.year(), today.month(), 1)
        .expect("the first day of the current month is always valid");

    match selection {
        PeriodSelection::MonthToDate => BillingPeriod {
            start: current_start,
            end: add_months(current_start, 1),
            month_to_date: true,
        },
        PeriodSelection::CompleteMonthsBefore(months) => BillingPeriod {
            start: add_months(current_start, -(months as i32)),
            end: current_start,
            month_to_date: false,
        },
        PeriodSelection::MonthsIncludingCurrent(months) => {
            assert!(months >= 1, "a period must include at least one month");

            BillingPeriod {
                start: add_months(current_start, -((months - 1) as i32)),
                end: add_months(current_start, 1),
                month_to_date: true,
            }
        }
    }
}

fn add_months(month_start: NaiveDate, months: i32) -> NaiveDate {
    let total_months = month_start.year() * 12 + month_start.month0() as i32 + months;
    let year = total_months.div_euclid(12);
    let month = total_months.rem_euclid(12) as u32 + 1;

    NaiveDate::from_ymd_opt(year, month, 1).expect("the first day of a month is always valid")
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

fn print_summary(summary: &BillingSummary) {
    println!(
        "{:<21}{}",
        "Organizational Cost",
        format_money(summary.organization_total, &summary.unit)
    );
    println!("{:<21}{}", "Period", summary.period.label());
    println!();

    if summary.accounts.is_empty() {
        println!("No costs have been recorded for the current billing period.");
    } else {
        print_breakdown_table(summary);
        println!();
        println!("Cost by Account — {}", summary.period.label());
        print_account_table(summary);
    }

    println!();
    if summary.estimated {
        println!(
            "Source: AWS Cost Explorer ({METRIC}, estimated). Data can lag by up to 24 hours."
        );
    } else {
        println!("Source: AWS Cost Explorer ({METRIC}). Data can lag by up to 24 hours.");
    }
}

fn print_breakdown_table(summary: &BillingSummary) {
    let mut account_width = "ACCOUNT".len();
    let mut service_width = "SERVICE".len().max("ACCOUNT TOTAL".len());
    let mut cost_width = "COST".len();

    for account in &summary.accounts {
        account_width = account_width.max(account_label(account).chars().count());
        cost_width = cost_width.max(format_money(account.amount, &summary.unit).len());

        for service in &account.services {
            service_width = service_width.max(service.name.chars().count());
            cost_width = cost_width.max(format_money(service.amount, &summary.unit).len());
        }
    }

    let widths = [account_width, service_width, cost_width];
    println!("{}", table_rule('┌', '┬', '┐', widths));
    println!(
        "│ {:<account_width$} │ {:<service_width$} │ {:>cost_width$} │",
        "ACCOUNT",
        "SERVICE",
        "COST",
        account_width = account_width,
        service_width = service_width,
        cost_width = cost_width
    );
    println!("{}", table_rule('├', '┼', '┤', widths));

    for (account_index, account) in summary.accounts.iter().enumerate() {
        for (service_index, service) in account.services.iter().enumerate() {
            let label = if service_index == 0 {
                account_label(account)
            } else {
                String::new()
            };

            println!(
                "│ {:<account_width$} │ {:<service_width$} │ {:>cost_width$} │",
                label,
                service.name,
                format_money(service.amount, &summary.unit),
                account_width = account_width,
                service_width = service_width,
                cost_width = cost_width
            );
        }

        println!("{}", table_rule('├', '┼', '┤', widths));
        println!(
            "│ {:<account_width$} │ {:<service_width$} │ {:>cost_width$} │",
            "",
            "ACCOUNT TOTAL",
            format_money(account.amount, &summary.unit),
            account_width = account_width,
            service_width = service_width,
            cost_width = cost_width
        );

        if account_index + 1 < summary.accounts.len() {
            println!("{}", table_rule('├', '┼', '┤', widths));
        }
    }

    println!("{}", table_rule('└', '┴', '┘', widths));
}

fn print_account_table(summary: &BillingSummary) {
    let mut name_width = "ACCOUNT".len();
    let mut id_width = "ACCOUNT ID".len();
    let mut cost_width = "COST"
        .len()
        .max(format_money(summary.organization_total, &summary.unit).len());

    for account in &summary.accounts {
        name_width = name_width.max(account_name(account).chars().count());
        id_width = id_width.max(account.account_id.chars().count());
        cost_width = cost_width.max(format_money(account.amount, &summary.unit).len());
    }

    let widths = [name_width, id_width, cost_width];
    println!("{}", table_rule('┌', '┬', '┐', widths));
    println!(
        "│ {:<name_width$} │ {:<id_width$} │ {:>cost_width$} │",
        "ACCOUNT",
        "ACCOUNT ID",
        "COST",
        name_width = name_width,
        id_width = id_width,
        cost_width = cost_width
    );
    println!("{}", table_rule('├', '┼', '┤', widths));

    for account in &summary.accounts {
        println!(
            "│ {:<name_width$} │ {:<id_width$} │ {:>cost_width$} │",
            account_name(account),
            account.account_id,
            format_money(account.amount, &summary.unit),
            name_width = name_width,
            id_width = id_width,
            cost_width = cost_width
        );
    }

    println!("{}", table_rule('├', '┼', '┤', widths));
    println!(
        "│ {:<name_width$} │ {:<id_width$} │ {:>cost_width$} │",
        "TOTAL",
        "",
        format_money(summary.organization_total, &summary.unit),
        name_width = name_width,
        id_width = id_width,
        cost_width = cost_width
    );
    println!("{}", table_rule('└', '┴', '┘', widths));
}

fn account_name(account: &AccountCost) -> &str {
    account
        .name
        .as_deref()
        .filter(|name| !name.is_empty())
        .unwrap_or("-")
}

fn account_label(account: &AccountCost) -> String {
    match account.name.as_deref() {
        Some(name) if !name.is_empty() => format!("{name} ({})", account.account_id),
        _ => account.account_id.clone(),
    }
}

fn table_rule(left: char, middle: char, right: char, widths: [usize; 3]) -> String {
    format!(
        "{left}{}{middle}{}{middle}{}{right}",
        "─".repeat(widths[0] + 2),
        "─".repeat(widths[1] + 2),
        "─".repeat(widths[2] + 2),
    )
}

fn format_money(amount: f64, unit: &str) -> String {
    let number = format_number(amount);

    match unit {
        "USD" => format!("${number}"),
        "EUR" => format!("€{number}"),
        "GBP" => format!("£{number}"),
        other => format!("{number} {other}"),
    }
}

fn format_number(amount: f64) -> String {
    let negative = amount < 0.0;
    let fixed = format!("{:.2}", amount.abs());
    let (whole, fraction) = fixed.split_once('.').unwrap_or((fixed.as_str(), "00"));

    let mut grouped = String::with_capacity(fixed.len() + whole.len() / 3);
    for (index, digit) in whole.chars().enumerate() {
        if index > 0 && (whole.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }

    if negative {
        format!("-{grouped}.{fraction}")
    } else {
        format!("{grouped}.{fraction}")
    }
}

#[derive(Debug, Clone, Copy)]
enum PeriodSelection {
    /// The current month up to today (the default).
    MonthToDate,
    /// That many complete calendar months before the current one.
    CompleteMonthsBefore(u32),
    /// That many months ending with the current one, which is partial.
    MonthsIncludingCurrent(u32),
}

#[derive(Debug, Clone, Copy)]
struct BillingPeriod {
    start: NaiveDate,
    /// Exclusive, as required by the Cost Explorer API.
    end: NaiveDate,
    /// Whether the period ends with the current, still-running month.
    month_to_date: bool,
}

impl BillingPeriod {
    fn label(&self) -> String {
        let last_day = self
            .end
            .pred_opt()
            .expect("a billing period always ends after it starts");
        let suffix = if self.month_to_date {
            " (month to date)"
        } else {
            ""
        };
        let same_month =
            self.start.year() == last_day.year() && self.start.month() == last_day.month();

        if same_month {
            format!("{}{suffix}", self.start.format("%B %Y"))
        } else if self.start.year() == last_day.year() {
            format!(
                "{} – {} {}{suffix}",
                self.start.format("%B"),
                last_day.format("%B"),
                last_day.format("%Y")
            )
        } else {
            format!(
                "{} – {}{suffix}",
                self.start.format("%B %Y"),
                last_day.format("%B %Y")
            )
        }
    }
}

#[derive(Debug)]
struct BillingSummary {
    period: BillingPeriod,
    unit: String,
    organization_total: f64,
    accounts: Vec<AccountCost>,
    estimated: bool,
}

#[derive(Debug)]
struct AccountCost {
    account_id: String,
    name: Option<String>,
    amount: f64,
    services: Vec<ServiceCost>,
}

#[derive(Debug)]
struct ServiceCost {
    name: String,
    amount: f64,
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;
    use std::time::Duration;

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

    #[test]
    fn billing_period_is_the_current_month() {
        let period = billing_period(
            NaiveDate::from_ymd_opt(2026, 10, 28).unwrap(),
            PeriodSelection::MonthToDate,
        );

        assert_eq!(period.start, NaiveDate::from_ymd_opt(2026, 10, 1).unwrap());
        assert_eq!(period.end, NaiveDate::from_ymd_opt(2026, 11, 1).unwrap());
        assert_eq!(period.label(), "October 2026 (month to date)");
    }

    #[test]
    fn billing_period_rolls_over_in_december() {
        let period = billing_period(
            NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(),
            PeriodSelection::MonthToDate,
        );

        assert_eq!(period.start, NaiveDate::from_ymd_opt(2026, 12, 1).unwrap());
        assert_eq!(period.end, NaiveDate::from_ymd_opt(2027, 1, 1).unwrap());
    }

    #[test]
    fn previous_month_excludes_the_current_month() {
        let period = billing_period(
            NaiveDate::from_ymd_opt(2026, 10, 28).unwrap(),
            PeriodSelection::CompleteMonthsBefore(1),
        );

        assert_eq!(period.start, NaiveDate::from_ymd_opt(2026, 9, 1).unwrap());
        assert_eq!(period.end, NaiveDate::from_ymd_opt(2026, 10, 1).unwrap());
        assert_eq!(period.label(), "September 2026");
    }

    #[test]
    fn last_three_months_exclude_the_current_month() {
        let period = billing_period(
            NaiveDate::from_ymd_opt(2026, 10, 28).unwrap(),
            PeriodSelection::CompleteMonthsBefore(3),
        );

        assert_eq!(period.start, NaiveDate::from_ymd_opt(2026, 7, 1).unwrap());
        assert_eq!(period.end, NaiveDate::from_ymd_opt(2026, 10, 1).unwrap());
        assert_eq!(period.label(), "July – September 2026");
    }

    #[test]
    fn last_six_months_cross_the_year_boundary() {
        let period = billing_period(
            NaiveDate::from_ymd_opt(2026, 2, 15).unwrap(),
            PeriodSelection::CompleteMonthsBefore(6),
        );

        assert_eq!(period.start, NaiveDate::from_ymd_opt(2025, 8, 1).unwrap());
        assert_eq!(period.end, NaiveDate::from_ymd_opt(2026, 2, 1).unwrap());
        assert_eq!(period.label(), "August 2025 – January 2026");
    }

    #[test]
    fn months_including_current_include_the_current_month() {
        let period = billing_period(
            NaiveDate::from_ymd_opt(2026, 10, 28).unwrap(),
            PeriodSelection::MonthsIncludingCurrent(3),
        );

        assert_eq!(period.start, NaiveDate::from_ymd_opt(2026, 8, 1).unwrap());
        assert_eq!(period.end, NaiveDate::from_ymd_opt(2026, 11, 1).unwrap());
        assert_eq!(period.label(), "August – October 2026 (month to date)");
    }

    #[test]
    fn one_month_including_current_matches_month_to_date() {
        let period = billing_period(
            NaiveDate::from_ymd_opt(2026, 10, 28).unwrap(),
            PeriodSelection::MonthsIncludingCurrent(1),
        );

        assert_eq!(period.start, NaiveDate::from_ymd_opt(2026, 10, 1).unwrap());
        assert_eq!(period.end, NaiveDate::from_ymd_opt(2026, 11, 1).unwrap());
        assert_eq!(period.label(), "October 2026 (month to date)");
    }

    #[test]
    fn months_including_current_cross_the_year_boundary() {
        let period = billing_period(
            NaiveDate::from_ymd_opt(2026, 2, 15).unwrap(),
            PeriodSelection::MonthsIncludingCurrent(6),
        );

        assert_eq!(period.start, NaiveDate::from_ymd_opt(2025, 9, 1).unwrap());
        assert_eq!(period.end, NaiveDate::from_ymd_opt(2026, 3, 1).unwrap());
        assert_eq!(
            period.label(),
            "September 2025 – February 2026 (month to date)"
        );
    }

    #[test]
    fn numbers_are_grouped_and_rounded() {
        assert_eq!(format_number(1_234_567.891), "1,234,567.89");
        assert_eq!(format_number(42.0), "42.00");
        assert_eq!(format_number(-1_234.5), "-1,234.50");
        assert_eq!(format_number(0.0), "0.00");
    }

    #[test]
    fn money_uses_the_metric_unit() {
        assert_eq!(format_money(12.5, "USD"), "$12.50");
        assert_eq!(format_money(12.5, "EUR"), "€12.50");
        assert_eq!(format_money(12.5, "GBP"), "£12.50");
        assert_eq!(format_money(12.5, "JPY"), "12.50 JPY");
    }

    #[tokio::test]
    async fn fetches_and_aggregates_a_cost_explorer_response() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let address = listener.local_addr().expect("read mock server address");

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .expect("set read timeout");

            let request = read_http_request(&mut stream);
            assert!(
                request.contains("AWSInsightsIndexService.GetCostAndUsage"),
                "unexpected request: {request}"
            );
            assert!(request.contains("LINKED_ACCOUNT"), "request: {request}");
            assert!(request.contains("SERVICE"), "request: {request}");
            assert!(!request.contains("\"Filter\""), "request: {request}");
            assert!(request.contains("2026-10-01"), "request: {request}");
            assert!(request.contains("2026-11-01"), "request: {request}");

            let body = r#"{
                "GroupDefinitions": [
                    {"Key": "LINKED_ACCOUNT", "Type": "DIMENSION"},
                    {"Key": "SERVICE", "Type": "DIMENSION"}
                ],
                "ResultsByTime": [{
                    "Estimated": true,
                    "Groups": [
                        {
                            "Keys": ["123456789012", "Amazon Elastic Compute Cloud - Compute"],
                            "Metrics": {"UnblendedCost": {"Amount": "10.50", "Unit": "USD"}}
                        },
                        {
                            "Keys": ["123456789012", "Amazon Simple Storage Service"],
                            "Metrics": {"UnblendedCost": {"Amount": "2.25", "Unit": "USD"}}
                        },
                        {
                            "Keys": ["999999999999", "Amazon Elastic Compute Cloud - Compute"],
                            "Metrics": {"UnblendedCost": {"Amount": "5.00", "Unit": "USD"}}
                        },
                        {
                            "Keys": ["123456789012", "AWS Lambda"],
                            "Metrics": {"UnblendedCost": {"Amount": "0.00", "Unit": "USD"}}
                        },
                        {
                            "Keys": ["777777777777", "Amazon Route 53"],
                            "Metrics": {"UnblendedCost": {"Amount": "0.00", "Unit": "USD"}}
                        }
                    ],
                    "TimePeriod": {"Start": "2026-10-01", "End": "2026-11-01"},
                    "Total": {}
                }]
            }"#;

            let response = format!(
                "HTTP/1.1 200 OK\r\n\
                 Content-Type: application/x-amz-json-1.1\r\n\
                 Content-Length: {}\r\n\
                 Connection: close\r\n\
                 \r\n\
                 {body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
            stream.flush().expect("flush response");
        });

        let config = aws_sdk_costexplorer::Config::builder()
            .behavior_version_latest()
            .region(Region::new("us-east-1"))
            .credentials_provider(aws_sdk_costexplorer::config::Credentials::new(
                "test-access-key",
                "test-secret-key",
                None,
                None,
                "test",
            ))
            .endpoint_url(format!("http://{address}"))
            .build();
        let client = CostExplorerClient::from_conf(config);
        let period = billing_period(
            NaiveDate::from_ymd_opt(2026, 10, 28).unwrap(),
            PeriodSelection::MonthToDate,
        );

        let account_names = HashMap::from([
            ("123456789012".to_owned(), "Production".to_owned()),
            ("999999999999".to_owned(), "Sandbox".to_owned()),
        ]);
        let summary = fetch_billing(&client, period, &account_names)
            .await
            .expect("fetch billing summary");

        server.join().expect("mock server thread panicked");

        assert_eq!(summary.unit, "USD");
        assert!(summary.estimated);
        assert!((summary.organization_total - 17.75).abs() < f64::EPSILON);

        assert_eq!(summary.accounts.len(), 2);
        assert_eq!(summary.accounts[0].account_id, "123456789012");
        assert_eq!(summary.accounts[0].name.as_deref(), Some("Production"));
        assert!((summary.accounts[0].amount - 12.75).abs() < f64::EPSILON);
        assert_eq!(summary.accounts[0].services.len(), 2);
        assert_eq!(
            summary.accounts[0].services[0].name,
            "Amazon Elastic Compute Cloud - Compute"
        );
        assert!((summary.accounts[0].services[0].amount - 10.5).abs() < f64::EPSILON);
        assert_eq!(
            summary.accounts[0].services[1].name,
            "Amazon Simple Storage Service"
        );
        assert!((summary.accounts[0].services[1].amount - 2.25).abs() < f64::EPSILON);

        assert_eq!(summary.accounts[1].account_id, "999999999999");
        assert_eq!(summary.accounts[1].name.as_deref(), Some("Sandbox"));
        assert!((summary.accounts[1].amount - 5.0).abs() < f64::EPSILON);
        assert_eq!(summary.accounts[1].services.len(), 1);
        assert_eq!(
            summary.accounts[1].services[0].name,
            "Amazon Elastic Compute Cloud - Compute"
        );
        assert!((summary.accounts[1].services[0].amount - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn costs_that_round_to_zero_are_hidden() {
        assert!(!is_displayable_cost(0.0));
        assert!(!is_displayable_cost(0.004));
        assert!(is_displayable_cost(0.01));
        assert!(is_displayable_cost(-0.01));
    }

    fn read_http_request(stream: &mut TcpStream) -> String {
        let mut data = Vec::new();
        let mut buffer = [0_u8; 4096];

        loop {
            let read = stream.read(&mut buffer).expect("read request");
            if read == 0 {
                break;
            }
            data.extend_from_slice(&buffer[..read]);

            if let Some(header_end) = data.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&data[..header_end]);
                let content_length = headers.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    if name.eq_ignore_ascii_case("content-length") {
                        value.trim().parse::<usize>().ok()
                    } else {
                        None
                    }
                });

                if data.len() >= header_end + 4 + content_length.unwrap_or_default() {
                    break;
                }
            }
        }

        String::from_utf8_lossy(&data).into_owned()
    }
}
