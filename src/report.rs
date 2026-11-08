//! Human-readable report tables and CSV export.

use std::fs::OpenOptions;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result};
use rust_decimal::Decimal;
use unicode_width::UnicodeWidthStr;

use crate::billing::{AccountCost, BillingSummary, round_money};

/// Write the formatted report tables.
pub fn render_summary(writer: &mut impl Write, summary: &BillingSummary) -> io::Result<()> {
    writeln!(
        writer,
        "{:<21}{}",
        "Organizational Cost",
        format_money(summary.organization_total, &summary.unit)
    )?;
    writeln!(writer, "{:<21}{}", "Period", summary.period.label())?;
    writeln!(writer)?;

    if summary.accounts.is_empty() {
        writeln!(
            writer,
            "No costs have been recorded for {}.",
            summary.period.label()
        )?;
    } else {
        render_breakdown_table(writer, summary)?;
        writeln!(writer)?;
        writeln!(writer, "Cost by Account — {}", summary.period.label())?;
        render_account_table(writer, summary)?;
    }

    writeln!(writer)?;
    if summary.estimated {
        writeln!(
            writer,
            "Source: AWS Cost Explorer ({}, estimated). Data can lag by up to 24 hours.",
            summary.metric.aws_name()
        )?;
    } else {
        writeln!(
            writer,
            "Source: AWS Cost Explorer ({}). Data can lag by up to 24 hours.",
            summary.metric.aws_name()
        )?;
    }

    Ok(())
}

fn render_breakdown_table(writer: &mut impl Write, summary: &BillingSummary) -> io::Result<()> {
    let mut account_width = "ACCOUNT".width();
    let mut service_width = "SERVICE".width().max("ACCOUNT TOTAL".width());
    let mut cost_width = "COST".width();

    for account in &summary.accounts {
        account_width = account_width.max(account_label(account).width());
        cost_width = cost_width.max(format_money(account.amount, &summary.unit).width());

        for service in &account.services {
            service_width = service_width.max(service.name.width());
            cost_width = cost_width.max(format_money(service.amount, &summary.unit).width());
        }
    }

    let widths = [account_width, service_width, cost_width];
    writeln!(writer, "{}", table_rule('┌', '┬', '┐', widths))?;
    writeln!(
        writer,
        "{}",
        padded_row(
            "ACCOUNT",
            account_width,
            "SERVICE",
            service_width,
            "COST",
            cost_width
        )
    )?;
    writeln!(writer, "{}", table_rule('├', '┼', '┤', widths))?;

    for (account_index, account) in summary.accounts.iter().enumerate() {
        for (service_index, service) in account.services.iter().enumerate() {
            let label = if service_index == 0 {
                account_label(account)
            } else {
                String::new()
            };

            writeln!(
                writer,
                "{}",
                padded_row(
                    &label,
                    account_width,
                    &service.name,
                    service_width,
                    &format_money(service.amount, &summary.unit),
                    cost_width,
                )
            )?;
        }

        writeln!(writer, "{}", table_rule('├', '┼', '┤', widths))?;
        writeln!(
            writer,
            "{}",
            padded_row(
                "",
                account_width,
                "ACCOUNT TOTAL",
                service_width,
                &format_money(account.amount, &summary.unit),
                cost_width,
            )
        )?;

        if account_index + 1 < summary.accounts.len() {
            writeln!(writer, "{}", table_rule('├', '┼', '┤', widths))?;
        }
    }

    writeln!(writer, "{}", table_rule('└', '┴', '┘', widths))
}

fn render_account_table(writer: &mut impl Write, summary: &BillingSummary) -> io::Result<()> {
    let mut name_width = "ACCOUNT".width();
    let mut id_width = "ACCOUNT ID".width();
    let mut cost_width = "COST"
        .width()
        .max(format_money(summary.organization_total, &summary.unit).width());

    for account in &summary.accounts {
        name_width = name_width.max(account_name(account).width());
        id_width = id_width.max(account.account_id.width());
        cost_width = cost_width.max(format_money(account.amount, &summary.unit).width());
    }

    let widths = [name_width, id_width, cost_width];
    writeln!(writer, "{}", table_rule('┌', '┬', '┐', widths))?;
    writeln!(
        writer,
        "{}",
        padded_row(
            "ACCOUNT",
            name_width,
            "ACCOUNT ID",
            id_width,
            "COST",
            cost_width
        )
    )?;
    writeln!(writer, "{}", table_rule('├', '┼', '┤', widths))?;

    for account in &summary.accounts {
        writeln!(
            writer,
            "{}",
            padded_row(
                account_name(account),
                name_width,
                &account.account_id,
                id_width,
                &format_money(account.amount, &summary.unit),
                cost_width,
            )
        )?;
    }

    writeln!(writer, "{}", table_rule('├', '┼', '┤', widths))?;
    writeln!(
        writer,
        "{}",
        padded_row(
            "TOTAL",
            name_width,
            "",
            id_width,
            &format_money(summary.organization_total, &summary.unit),
            cost_width,
        )
    )?;
    writeln!(writer, "{}", table_rule('└', '┴', '┘', widths))
}

/// Write the service cost table as RFC 4180 CSV.
pub fn write_csv(writer: &mut impl Write, summary: &BillingSummary) -> io::Result<()> {
    write!(
        writer,
        "period,period_start,period_end,account_name,account_id,service,cost,unit\r\n"
    )?;

    let period = summary.period.label();
    let period_start = summary.period.start().format("%Y-%m-%d").to_string();
    let period_end = summary.period.end().format("%Y-%m-%d").to_string();
    for account in &summary.accounts {
        for service in &account.services {
            write!(
                writer,
                "{},{},{},{},{},{},{:.2},{}\r\n",
                csv_field(&period),
                period_start,
                period_end,
                csv_field(account.name.as_deref().unwrap_or("")),
                csv_field(&account.account_id),
                csv_field(&service.name),
                service.amount,
                csv_field(&summary.unit),
            )?;
        }
    }

    Ok(())
}

/// Write a CSV export to `path`, creating or truncating the file with owner-only
/// permissions on Unix.
pub fn write_csv_file(path: &Path, summary: &BillingSummary) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let file = options
        .open(path)
        .with_context(|| format!("failed to create the CSV file {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .with_context(|| {
                format!(
                    "failed to restrict permissions on the CSV file {}",
                    path.display()
                )
            })?;
    }

    file.set_len(0)
        .with_context(|| format!("failed to truncate the CSV file {}", path.display()))?;

    let mut writer = BufWriter::new(file);
    write_csv(&mut writer, summary)
        .with_context(|| format!("failed to write the CSV file {}", path.display()))?;
    writer
        .flush()
        .with_context(|| format!("failed to flush the CSV file {}", path.display()))
}

/// Quote a CSV field when it contains a comma, quote, or line break. Embedded
/// quotes are doubled, as required by RFC 4180. Fields that a spreadsheet could
/// interpret as formulas are prefixed with an apostrophe.
fn csv_field(value: &str) -> String {
    let mut value = value.to_owned();

    if value
        .chars()
        .next()
        .is_some_and(|first| matches!(first, '=' | '+' | '-' | '@' | '\t' | '\r'))
    {
        value.insert(0, '\'');
    }

    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value
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

fn padded_row(
    left: &str,
    left_width: usize,
    middle: &str,
    middle_width: usize,
    right: &str,
    right_width: usize,
) -> String {
    format!(
        "│ {} │ {} │ {} │",
        pad_right(left, left_width),
        pad_right(middle, middle_width),
        pad_left(right, right_width)
    )
}

fn pad_right(value: &str, width: usize) -> String {
    let padding = width.saturating_sub(value.width());
    format!("{value}{}", " ".repeat(padding))
}

fn pad_left(value: &str, width: usize) -> String {
    let padding = width.saturating_sub(value.width());
    format!("{}{value}", " ".repeat(padding))
}

fn format_money(amount: Decimal, unit: &str) -> String {
    let formatted = format_number(amount);
    let (sign, number) = formatted
        .strip_prefix('-')
        .map_or(("", formatted.as_str()), |rest| ("-", rest));

    match unit {
        "USD" => format!("{sign}${number}"),
        "EUR" => format!("{sign}€{number}"),
        "GBP" => format!("{sign}£{number}"),
        other => format!("{sign}{number} {other}"),
    }
}

fn format_number(amount: Decimal) -> String {
    // Round before choosing the sign so amounts below half a cent do not render
    // as `-$0.00`.
    let rounded = round_money(amount);
    let negative = rounded.is_sign_negative() && !rounded.is_zero();
    let fixed = format!("{:.2}", rounded.abs());
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
    use tempfile::tempdir;

    use super::*;
    use crate::billing::{BillingPeriod, Metric, ServiceCost};

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("valid test date")
    }

    fn dec(value: &str) -> Decimal {
        value.parse().expect("valid test decimal")
    }

    fn test_period() -> BillingPeriod {
        BillingPeriod::new(date(2026, 10, 1), date(2026, 11, 1), true).expect("valid period")
    }

    fn summary(accounts: Vec<AccountCost>) -> BillingSummary {
        let organization_total = accounts.iter().map(|account| account.amount).sum();
        BillingSummary {
            period: test_period(),
            metric: Metric::Unblended,
            unit: "USD".to_owned(),
            organization_total,
            accounts,
            estimated: true,
        }
    }

    #[test]
    fn numbers_are_grouped_and_rounded() {
        assert_eq!(format_number(dec("1234567.891")), "1,234,567.89");
        assert_eq!(format_number(dec("42")), "42.00");
        assert_eq!(format_number(dec("-1234.5")), "-1,234.50");
        assert_eq!(format_number(Decimal::ZERO), "0.00");
    }

    #[test]
    fn money_uses_the_metric_unit() {
        assert_eq!(format_money(dec("12.5"), "USD"), "$12.50");
        assert_eq!(format_money(dec("12.5"), "EUR"), "€12.50");
        assert_eq!(format_money(dec("12.5"), "GBP"), "£12.50");
        assert_eq!(format_money(dec("12.5"), "JPY"), "12.50 JPY");
    }

    #[test]
    fn negative_money_puts_the_sign_before_the_symbol() {
        assert_eq!(format_money(dec("-12.5"), "USD"), "-$12.50");
        assert_eq!(format_money(dec("-12.5"), "EUR"), "-€12.50");
        assert_eq!(format_money(dec("-12.5"), "GBP"), "-£12.50");
        assert_eq!(format_money(dec("-12.5"), "JPY"), "-12.50 JPY");
        assert_eq!(format_money(dec("-0.001"), "USD"), "$0.00");
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
    fn csv_fields_cannot_start_a_spreadsheet_formula() {
        assert_eq!(csv_field("=SUM(A1)"), "'=SUM(A1)");
        assert_eq!(csv_field("+1"), "'+1");
        assert_eq!(csv_field("-1"), "'-1");
        assert_eq!(csv_field("@cmd"), "'@cmd");
        assert_eq!(csv_field("=1,2"), "\"'=1,2\"");
    }

    #[test]
    fn csv_export_has_a_row_per_service() {
        let summary = summary(vec![
            AccountCost {
                account_id: "123456789012".to_owned(),
                name: Some("Production, Inc.".to_owned()),
                amount: dec("12.75"),
                services: vec![
                    ServiceCost {
                        name: "Amazon \"EC2\"".to_owned(),
                        amount: dec("10.50"),
                    },
                    ServiceCost {
                        name: "Amazon S3".to_owned(),
                        amount: dec("2.25"),
                    },
                ],
            },
            AccountCost {
                account_id: "999999999999".to_owned(),
                name: None,
                amount: dec("5.00"),
                services: vec![ServiceCost {
                    name: "AWS Lambda".to_owned(),
                    amount: dec("5.00"),
                }],
            },
        ]);

        let mut buffer = Vec::new();
        write_csv(&mut buffer, &summary).expect("write CSV");

        let csv = String::from_utf8(buffer).expect("CSV is UTF-8");
        assert_eq!(
            csv,
            concat!(
                "period,period_start,period_end,account_name,account_id,service,cost,unit\r\n",
                "October 2026 (month to date),2026-10-01,2026-11-01,\"Production, Inc.\",123456789012,\"Amazon \"\"EC2\"\"\",10.50,USD\r\n",
                "October 2026 (month to date),2026-10-01,2026-11-01,\"Production, Inc.\",123456789012,Amazon S3,2.25,USD\r\n",
                "October 2026 (month to date),2026-10-01,2026-11-01,,999999999999,AWS Lambda,5.00,USD\r\n",
            )
        );
    }

    #[test]
    fn csv_export_without_costs_writes_only_the_header() {
        let summary = summary(Vec::new());

        let mut buffer = Vec::new();
        write_csv(&mut buffer, &summary).expect("write CSV");

        assert_eq!(
            String::from_utf8(buffer).expect("CSV is UTF-8"),
            "period,period_start,period_end,account_name,account_id,service,cost,unit\r\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn csv_file_has_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempdir().expect("create temporary directory");
        let path = directory.path().join("costs.csv");

        write_csv_file(&path, &summary(Vec::new())).expect("write CSV file");

        let mode = std::fs::metadata(&path)
            .expect("read CSV metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn csv_file_tightens_existing_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempdir().expect("create temporary directory");
        let path = directory.path().join("costs.csv");
        std::fs::write(&path, "old").expect("write old CSV");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("set old permissions");

        write_csv_file(&path, &summary(Vec::new())).expect("overwrite CSV file");

        let mode = std::fs::metadata(&path)
            .expect("read CSV metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(
            std::fs::read_to_string(&path).expect("read CSV"),
            "period,period_start,period_end,account_name,account_id,service,cost,unit\r\n"
        );
    }

    #[test]
    fn renders_tables_with_negative_money() {
        let summary = summary(vec![AccountCost {
            account_id: "123456789012".to_owned(),
            name: Some("Production".to_owned()),
            amount: dec("-12.50"),
            services: vec![ServiceCost {
                name: "AWS Credits".to_owned(),
                amount: dec("-12.50"),
            }],
        }]);

        let mut buffer = Vec::new();
        render_summary(&mut buffer, &summary).expect("render summary");
        let output = String::from_utf8(buffer).expect("table is UTF-8");

        assert!(output.contains("Organizational Cost  -$12.50"));
        assert!(output.contains("Production (123456789012)"));
        assert!(output.contains("ACCOUNT TOTAL"));
        assert!(output.contains("Cost by Account — October 2026 (month to date)"));
        assert!(output.contains("Source: AWS Cost Explorer (UnblendedCost, estimated)"));
    }

    #[test]
    fn empty_report_names_the_selected_period() {
        let mut summary = summary(Vec::new());
        summary.period =
            BillingPeriod::new(date(2026, 9, 1), date(2026, 10, 1), false).expect("valid period");

        let mut buffer = Vec::new();
        render_summary(&mut buffer, &summary).expect("render summary");
        let output = String::from_utf8(buffer).expect("table is UTF-8");

        assert!(output.contains("No costs have been recorded for September 2026."));
    }

    #[test]
    fn unicode_width_padding_aligns_wide_characters() {
        assert_eq!(pad_right("日本", 6), "日本  ");
        assert_eq!(pad_right("abc", 6), "abc   ");
        assert_eq!(pad_left("$1.00", 8), "   $1.00");
        assert_eq!(pad_left("€1.00", 8), "   €1.00");
    }
}
