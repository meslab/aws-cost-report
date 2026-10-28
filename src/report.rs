//! Human-readable report tables and CSV export.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result};

use crate::METRIC;
use crate::billing::{AccountCost, BillingSummary};

pub fn print_summary(summary: &BillingSummary) {
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
        println!("Cost by Account: {}", summary.period.label());
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

pub fn write_csv(path: &Path, summary: &BillingSummary) -> Result<()> {
    let file = File::create(path)
        .with_context(|| format!("failed to create the CSV file {}", path.display()))?;
    let mut writer = BufWriter::new(file);

    write_csv_to(&mut writer, summary)
        .with_context(|| format!("failed to write the CSV file {}", path.display()))?;

    writer
        .flush()
        .with_context(|| format!("failed to flush the CSV file {}", path.display()))
}

fn write_csv_to(writer: &mut impl Write, summary: &BillingSummary) -> Result<()> {
    writeln!(writer, "period,account_name,account_id,service,cost,unit")
        .context("failed to write the CSV header")?;

    let period = summary.period.label();
    for account in &summary.accounts {
        for service in &account.services {
            writeln!(
                writer,
                "{},{},{},{},{:.2},{}",
                csv_field(&period),
                csv_field(account.name.as_deref().unwrap_or("")),
                csv_field(&account.account_id),
                csv_field(&service.name),
                service.amount,
                csv_field(&summary.unit),
            )
            .context("failed to write a CSV row")?;
        }
    }

    Ok(())
}

/// Quote a CSV field when it contains a comma, quote, or line break. Embedded
/// quotes are doubled, as required by RFC 4180.
fn csv_field(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
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

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;
    use crate::billing::{BillingPeriod, ServiceCost};

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

    #[test]
    fn csv_fields_are_escaped() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("with,comma"), "\"with,comma\"");
        assert_eq!(csv_field("with \"quotes\""), "\"with \"\"quotes\"\"\"");
        assert_eq!(csv_field("with\nnewline"), "\"with\nnewline\"");
        assert_eq!(csv_field("with\r\nnewline"), "\"with\r\nnewline\"");
    }

    #[test]
    fn csv_export_has_a_row_per_service() {
        let summary = BillingSummary {
            period: BillingPeriod {
                start: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
                end: NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(),
                month_to_date: true,
            },
            unit: "USD".to_owned(),
            organization_total: 17.75,
            accounts: vec![
                AccountCost {
                    account_id: "123456789012".to_owned(),
                    name: Some("Production, Inc.".to_owned()),
                    amount: 12.75,
                    services: vec![
                        ServiceCost {
                            name: "Amazon \"EC2\"".to_owned(),
                            amount: 10.5,
                        },
                        ServiceCost {
                            name: "Amazon S3".to_owned(),
                            amount: 2.25,
                        },
                    ],
                },
                AccountCost {
                    account_id: "999999999999".to_owned(),
                    name: None,
                    amount: 5.0,
                    services: vec![ServiceCost {
                        name: "AWS Lambda".to_owned(),
                        amount: 5.0,
                    }],
                },
            ],
            estimated: true,
        };

        let mut buffer = Vec::new();
        write_csv_to(&mut buffer, &summary).expect("write CSV");

        let csv = String::from_utf8(buffer).expect("CSV is UTF-8");
        assert_eq!(
            csv,
            concat!(
                "period,account_name,account_id,service,cost,unit\n",
                "October 2026 (month to date),\"Production, Inc.\",123456789012,\"Amazon \"\"EC2\"\"\",10.50,USD\n",
                "October 2026 (month to date),\"Production, Inc.\",123456789012,Amazon S3,2.25,USD\n",
                "October 2026 (month to date),,999999999999,AWS Lambda,5.00,USD\n",
            )
        );
    }

    #[test]
    fn csv_export_without_costs_writes_only_the_header() {
        let summary = BillingSummary {
            period: BillingPeriod {
                start: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
                end: NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(),
                month_to_date: true,
            },
            unit: "USD".to_owned(),
            organization_total: 0.0,
            accounts: Vec::new(),
            estimated: false,
        };

        let mut buffer = Vec::new();
        write_csv_to(&mut buffer, &summary).expect("write CSV");

        assert_eq!(
            String::from_utf8(buffer).expect("CSV is UTF-8"),
            "period,account_name,account_id,service,cost,unit\n"
        );
    }
}
