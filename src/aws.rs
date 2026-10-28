//! AWS SDK calls for account names and cost data.

use std::collections::HashMap;

use anyhow::{Context, Result, anyhow, ensure};
use aws_config::{BehaviorVersion, Region, SdkConfig};
use aws_sdk_costexplorer::Client as CostExplorerClient;
use aws_sdk_costexplorer::types::{
    DateInterval, Granularity, GroupDefinition, GroupDefinitionType,
};
use aws_sdk_organizations::Client as OrganizationsClient;

use crate::METRIC;
use crate::billing::{
    AccountCost, BillingPeriod, BillingSummary, ServiceCost, is_displayable_cost,
};

/// Cost Explorer's API is hosted in us-east-1, regardless of where the
/// account's resources live.
const COST_EXPLORER_REGION: &str = "us-east-1";

pub(crate) async fn fetch_account_names(
    client: &OrganizationsClient,
) -> Result<HashMap<String, String>> {
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

pub(crate) async fn load_config(profile: &str) -> SdkConfig {
    aws_config::defaults(BehaviorVersion::latest())
        .region(Region::new(COST_EXPLORER_REGION))
        .profile_name(profile)
        .load()
        .await
}

pub(crate) async fn fetch_billing(
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

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;
    use std::time::Duration;

    use chrono::NaiveDate;

    use super::*;
    use crate::billing::{PeriodSelection, billing_period};

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
