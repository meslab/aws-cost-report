//! Billing periods, metrics, and the aggregated cost model.
//!
//! Cost Explorer returns costs for a date range. These types select that range,
//! choose which cost metric to request, and hold the aggregated results that
//! the report renders.

use std::error::Error;
use std::fmt;
use std::str::FromStr;

use chrono::{Datelike, NaiveDate};
use rust_decimal::{Decimal, RoundingStrategy};

/// The Cost Explorer metric reported by the app.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Metric {
    /// Usage cost before credits and refunds.
    #[default]
    Unblended,
    /// Usage cost after credits and refunds.
    NetUnblended,
}

impl Metric {
    /// The exact metric name sent to Cost Explorer.
    pub fn aws_name(self) -> &'static str {
        match self {
            Self::Unblended => "UnblendedCost",
            Self::NetUnblended => "NetUnblendedCost",
        }
    }
}

impl fmt::Display for Metric {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unblended => "unblended",
            Self::NetUnblended => "net-unblended",
        })
    }
}

impl FromStr for Metric {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().replace('_', "-").as_str() {
            "unblended" | "unblended-cost" | "unblendedcost" => Ok(Self::Unblended),
            "net" | "net-unblended" | "net-unblended-cost" | "netunblendedcost" => {
                Ok(Self::NetUnblended)
            }
            _ => Err("metric must be `unblended` or `net-unblended`".to_owned()),
        }
    }
}

/// How the report period is selected from the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeriodSelection {
    /// The current month up to today (the default).
    MonthToDate,
    /// That many complete calendar months before the current one.
    CompleteMonthsBefore(u32),
    /// That many months ending with the current one, which is partial.
    MonthsIncludingCurrent(u32),
}

/// A start-inclusive, end-exclusive range of dates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BillingPeriod {
    start: NaiveDate,
    end: NaiveDate,
    month_to_date: bool,
}

impl BillingPeriod {
    /// Build a period, rejecting empty or reversed ranges.
    pub fn new(
        start: NaiveDate,
        end: NaiveDate,
        month_to_date: bool,
    ) -> Result<Self, BillingPeriodError> {
        if end <= start {
            return Err(BillingPeriodError::EmptyRange);
        }

        Ok(Self {
            start,
            end,
            month_to_date,
        })
    }

    /// The first day included in the period.
    pub fn start(self) -> NaiveDate {
        self.start
    }

    /// The first day *after* the period, as required by Cost Explorer.
    pub fn end(self) -> NaiveDate {
        self.end
    }

    /// Whether the period ends with the current, still-running month.
    pub fn month_to_date(self) -> bool {
        self.month_to_date
    }

    /// A human-readable label such as "September 2026" or
    /// "July – September 2026".
    pub fn label(self) -> String {
        let last_day = self.end.pred_opt().unwrap_or(self.start);
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

/// Errors from building a billing period.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BillingPeriodError {
    /// A relative period must include at least one month.
    ZeroMonths,
    /// The computed end date was not after the start date.
    EmptyRange,
    /// The requested range falls outside the supported calendar dates.
    DateOutOfRange,
}

impl fmt::Display for BillingPeriodError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ZeroMonths => "a billing period must include at least one month",
            Self::EmptyRange => "a billing period must end after it starts",
            Self::DateOutOfRange => "the billing period is outside the supported date range",
        })
    }
}

impl Error for BillingPeriodError {}

/// Aggregated costs for the selected period.
#[derive(Debug, Clone)]
pub struct BillingSummary {
    pub period: BillingPeriod,
    pub metric: Metric,
    pub unit: String,
    pub organization_total: Decimal,
    pub accounts: Vec<AccountCost>,
    pub estimated: bool,
}

/// Costs for one linked account, broken down by service.
#[derive(Debug, Clone)]
pub struct AccountCost {
    pub account_id: String,
    pub name: Option<String>,
    pub amount: Decimal,
    pub services: Vec<ServiceCost>,
}

/// Costs for one AWS service within an account.
#[derive(Debug, Clone)]
pub struct ServiceCost {
    pub name: String,
    pub amount: Decimal,
}

/// Turn a date and period selection into the Cost Explorer date range.
pub fn billing_period(
    today: NaiveDate,
    selection: PeriodSelection,
) -> Result<BillingPeriod, BillingPeriodError> {
    let current_start = NaiveDate::from_ymd_opt(today.year(), today.month(), 1)
        .ok_or(BillingPeriodError::DateOutOfRange)?;

    match selection {
        PeriodSelection::MonthToDate => {
            BillingPeriod::new(current_start, add_months(current_start, 1)?, true)
        }
        PeriodSelection::CompleteMonthsBefore(months) => {
            if months == 0 {
                return Err(BillingPeriodError::ZeroMonths);
            }

            BillingPeriod::new(
                add_months(current_start, -i64::from(months))?,
                current_start,
                false,
            )
        }
        PeriodSelection::MonthsIncludingCurrent(months) => {
            if months == 0 {
                return Err(BillingPeriodError::ZeroMonths);
            }

            BillingPeriod::new(
                add_months(current_start, -i64::from(months - 1))?,
                add_months(current_start, 1)?,
                true,
            )
        }
    }
}

fn add_months(month_start: NaiveDate, months: i64) -> Result<NaiveDate, BillingPeriodError> {
    let total_months = i64::from(month_start.year())
        .checked_mul(12)
        .and_then(|value| value.checked_add(i64::from(month_start.month0())))
        .and_then(|value| value.checked_add(months))
        .ok_or(BillingPeriodError::DateOutOfRange)?;
    let year = i32::try_from(total_months.div_euclid(12))
        .map_err(|_| BillingPeriodError::DateOutOfRange)?;
    let month = total_months.rem_euclid(12) as u32 + 1;

    NaiveDate::from_ymd_opt(year, month, 1).ok_or(BillingPeriodError::DateOutOfRange)
}

/// Round a monetary amount to the two decimal places shown in the report.
///
/// Rounding happens once, before filtering and aggregation, so the rendered
/// service rows, account totals, CSV rows, and organization total agree.
pub(crate) fn round_money(amount: Decimal) -> Decimal {
    // Money rounds half away from zero (0.005 -> 0.01, -0.005 -> -0.01).
    // `Decimal::round_dp` uses banker's rounding, which would round 0.005 to
    // 0.00 and make the displayed cents disagree with the expected bill.
    amount.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("valid test date")
    }

    fn dec(value: &str) -> Decimal {
        value.parse().expect("valid test decimal")
    }

    #[test]
    fn billing_period_is_the_current_month() {
        let period =
            billing_period(date(2026, 10, 28), PeriodSelection::MonthToDate).expect("valid period");

        assert_eq!(period.start(), date(2026, 10, 1));
        assert_eq!(period.end(), date(2026, 11, 1));
        assert!(period.month_to_date());
        assert_eq!(period.label(), "October 2026 (month to date)");
    }

    #[test]
    fn billing_period_rolls_over_in_december() {
        let period =
            billing_period(date(2026, 12, 31), PeriodSelection::MonthToDate).expect("valid period");

        assert_eq!(period.start(), date(2026, 12, 1));
        assert_eq!(period.end(), date(2027, 1, 1));
    }

    #[test]
    fn previous_month_excludes_the_current_month() {
        let period = billing_period(date(2026, 10, 28), PeriodSelection::CompleteMonthsBefore(1))
            .expect("valid period");

        assert_eq!(period.start(), date(2026, 9, 1));
        assert_eq!(period.end(), date(2026, 10, 1));
        assert_eq!(period.label(), "September 2026");
    }

    #[test]
    fn last_three_months_exclude_the_current_month() {
        let period = billing_period(date(2026, 10, 28), PeriodSelection::CompleteMonthsBefore(3))
            .expect("valid period");

        assert_eq!(period.start(), date(2026, 7, 1));
        assert_eq!(period.end(), date(2026, 10, 1));
        assert_eq!(period.label(), "July – September 2026");
    }

    #[test]
    fn last_six_months_cross_the_year_boundary() {
        let period = billing_period(date(2026, 2, 15), PeriodSelection::CompleteMonthsBefore(6))
            .expect("valid period");

        assert_eq!(period.start(), date(2025, 8, 1));
        assert_eq!(period.end(), date(2026, 2, 1));
        assert_eq!(period.label(), "August 2025 – January 2026");
    }

    #[test]
    fn months_including_current_include_the_current_month() {
        let period = billing_period(
            date(2026, 10, 28),
            PeriodSelection::MonthsIncludingCurrent(3),
        )
        .expect("valid period");

        assert_eq!(period.start(), date(2026, 8, 1));
        assert_eq!(period.end(), date(2026, 11, 1));
        assert_eq!(period.label(), "August – October 2026 (month to date)");
    }

    #[test]
    fn one_month_including_current_matches_month_to_date() {
        let period = billing_period(
            date(2026, 10, 28),
            PeriodSelection::MonthsIncludingCurrent(1),
        )
        .expect("valid period");

        assert_eq!(period.start(), date(2026, 10, 1));
        assert_eq!(period.end(), date(2026, 11, 1));
        assert_eq!(period.label(), "October 2026 (month to date)");
    }

    #[test]
    fn months_including_current_cross_the_year_boundary() {
        let period = billing_period(
            date(2026, 2, 15),
            PeriodSelection::MonthsIncludingCurrent(6),
        )
        .expect("valid period");

        assert_eq!(period.start(), date(2025, 9, 1));
        assert_eq!(period.end(), date(2026, 3, 1));
        assert_eq!(
            period.label(),
            "September 2025 – February 2026 (month to date)"
        );
    }

    #[test]
    fn zero_month_selections_are_rejected() {
        assert_eq!(
            billing_period(date(2026, 10, 28), PeriodSelection::CompleteMonthsBefore(0)),
            Err(BillingPeriodError::ZeroMonths)
        );
        assert_eq!(
            billing_period(
                date(2026, 10, 28),
                PeriodSelection::MonthsIncludingCurrent(0)
            ),
            Err(BillingPeriodError::ZeroMonths)
        );
    }

    #[test]
    fn huge_month_selections_are_rejected_without_panicking() {
        assert_eq!(
            billing_period(
                date(2026, 10, 28),
                PeriodSelection::CompleteMonthsBefore(u32::MAX)
            ),
            Err(BillingPeriodError::DateOutOfRange)
        );
        assert_eq!(
            billing_period(
                date(2026, 10, 28),
                PeriodSelection::MonthsIncludingCurrent(u32::MAX)
            ),
            Err(BillingPeriodError::DateOutOfRange)
        );
    }

    #[test]
    fn empty_ranges_are_rejected() {
        assert_eq!(
            BillingPeriod::new(date(2026, 10, 1), date(2026, 10, 1), false),
            Err(BillingPeriodError::EmptyRange)
        );
        assert_eq!(
            BillingPeriod::new(date(2026, 10, 2), date(2026, 10, 1), false),
            Err(BillingPeriodError::EmptyRange)
        );
    }

    #[test]
    fn money_is_rounded_to_cents() {
        assert_eq!(round_money(dec("0.004")), Decimal::ZERO);
        assert_eq!(round_money(dec("-0.004")), Decimal::ZERO);
        assert_eq!(round_money(dec("0.005")), dec("0.01"));
        assert_eq!(round_money(dec("-0.005")), dec("-0.01"));
        assert_eq!(round_money(dec("1.234")), dec("1.23"));
    }

    #[test]
    fn metric_names_and_parsing() {
        assert_eq!(Metric::Unblended.aws_name(), "UnblendedCost");
        assert_eq!(Metric::NetUnblended.aws_name(), "NetUnblendedCost");
        assert_eq!("unblended".parse::<Metric>(), Ok(Metric::Unblended));
        assert_eq!("UnblendedCost".parse::<Metric>(), Ok(Metric::Unblended));
        assert_eq!("net-unblended".parse::<Metric>(), Ok(Metric::NetUnblended));
        assert_eq!(
            "NetUnblendedCost".parse::<Metric>(),
            Ok(Metric::NetUnblended)
        );
        assert!("gross".parse::<Metric>().is_err());
    }
}
