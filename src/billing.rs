//! Billing periods and the aggregated cost model.
//!
//! Cost Explorer returns costs for a date range. These types select that range
//! and hold the aggregated results that the report renders.

use chrono::{Datelike, NaiveDate};

/// How the report period is selected from the command line.
#[derive(Debug, Clone, Copy)]
pub enum PeriodSelection {
    /// The current month up to today (the default).
    MonthToDate,
    /// That many complete calendar months before the current one.
    CompleteMonthsBefore(u32),
    /// That many months ending with the current one, which is partial.
    MonthsIncludingCurrent(u32),
}

/// A start-inclusive, end-exclusive range of calendar months.
#[derive(Debug, Clone, Copy)]
pub struct BillingPeriod {
    pub start: NaiveDate,
    /// Exclusive, as required by the Cost Explorer API.
    pub end: NaiveDate,
    /// Whether the period ends with the current, still-running month.
    pub month_to_date: bool,
}

impl BillingPeriod {
    /// A human-readable label such as "September 2026" or
    /// "July – September 2026".
    pub fn label(&self) -> String {
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

/// Aggregated costs for the selected period.
#[derive(Debug)]
pub struct BillingSummary {
    pub period: BillingPeriod,
    pub unit: String,
    pub organization_total: f64,
    pub accounts: Vec<AccountCost>,
    pub estimated: bool,
}

/// Costs for one linked account, broken down by service.
#[derive(Debug)]
pub struct AccountCost {
    pub account_id: String,
    pub name: Option<String>,
    pub amount: f64,
    pub services: Vec<ServiceCost>,
}

/// Costs for one AWS service within an account.
#[derive(Debug)]
pub struct ServiceCost {
    pub name: String,
    pub amount: f64,
}

/// Turn a date and period selection into the Cost Explorer date range.
pub fn billing_period(today: NaiveDate, selection: PeriodSelection) -> BillingPeriod {
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

/// Cost Explorer can return explicit zero-cost groups. Hide amounts that would
/// render as $0.00, such as a service with no usage this month.
pub(crate) fn is_displayable_cost(amount: f64) -> bool {
    amount.abs() >= 0.005
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn costs_that_round_to_zero_are_hidden() {
        assert!(!is_displayable_cost(0.0));
        assert!(!is_displayable_cost(0.004));
        assert!(is_displayable_cost(0.01));
        assert!(is_displayable_cost(-0.01));
    }
}
