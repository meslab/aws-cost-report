//! AWS SDK calls for account names and cost data.

use std::collections::HashMap;

use anyhow::{Context, Result, anyhow, ensure};
use aws_config::{BehaviorVersion, Region, SdkConfig};
use aws_sdk_costexplorer::Client as CostExplorerClient;
use aws_sdk_costexplorer::types::{
    DateInterval, Granularity, GroupDefinition, GroupDefinitionType,
};
use aws_sdk_organizations::Client as OrganizationsClient;
use rust_decimal::Decimal;

use crate::billing::{
    AccountCost, BillingPeriod, BillingSummary, Metric, ServiceCost, round_money,
};

/// Cost Explorer's API is hosted in us-east-1, regardless of where the
/// account's resources live.
const COST_EXPLORER_REGION: &str = "us-east-1";

/// Raw costs grouped by linked account and service.
#[derive(Debug)]
pub(crate) struct CostData {
    pub(crate) unit: Option<String>,
    pub(crate) estimated: bool,
    pub(crate) account_service_costs: HashMap<String, HashMap<String, Decimal>>,
}

/// Fetch every account name visible to the caller.
///
/// `ListAccounts` paginates with `NextToken`, so each request depends on the
/// previous response and the pages must be fetched in order. The caller runs
/// this lookup concurrently with [`fetch_costs`].
pub(crate) async fn fetch_account_names(
    client: &OrganizationsClient,
) -> Result<HashMap<String, String>> {
    let mut names = HashMap::new();
    let mut next_token: Option<String> = None;

    loop {
        let mut request = client.list_accounts();

        if let Some(token) = next_token.take() {
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

        next_token = response.next_token().map(str::to_owned);
        if next_token.is_none() {
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

pub(crate) async fn fetch_costs(
    client: &CostExplorerClient,
    period: BillingPeriod,
    metric: Metric,
) -> Result<CostData> {
    let metric_name = metric.aws_name();
    let time_period = DateInterval::builder()
        .start(period.start().format("%Y-%m-%d").to_string())
        .end(period.end().format("%Y-%m-%d").to_string())
        .build()
        .context("failed to build the Cost Explorer time period")?;

    let mut account_service_costs: HashMap<String, HashMap<String, Decimal>> = HashMap::new();
    let mut unit: Option<String> = None;
    let mut estimated = false;
    let mut next_page_token: Option<String> = None;

    // `GetCostAndUsage` returns the token for the next page only after the
    // current page is fetched, so pagination is inherently sequential. The
    // caller runs this query concurrently with the account-name lookup.
    loop {
        let mut request = client
            .get_cost_and_usage()
            .time_period(time_period.clone())
            .granularity(Granularity::Monthly)
            .metrics(metric_name)
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
                    .and_then(|metrics| metrics.get(metric_name))
                    .ok_or_else(|| {
                        anyhow!("Cost Explorer returned a group without a {metric_name} metric")
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
                        anyhow!("Cost Explorer returned a {metric_name} metric without an amount")
                    })?
                    .parse::<Decimal>()
                    .with_context(|| {
                        format!(
                            "Cost Explorer returned an invalid {metric_name} amount for {service} \
                             in account {linked_account}"
                        )
                    })?;

                *account_service_costs
                    .entry(linked_account.clone())
                    .or_default()
                    .entry(service)
                    .or_insert(Decimal::ZERO) += amount;
            }
        }

        next_page_token = response.next_page_token().map(str::to_owned);
        if next_page_token.is_none() {
            break;
        }
    }

    Ok(CostData {
        unit,
        estimated,
        account_service_costs,
    })
}

pub(crate) fn assemble_summary(
    period: BillingPeriod,
    metric: Metric,
    data: CostData,
    account_names: &HashMap<String, String>,
) -> BillingSummary {
    let mut accounts: Vec<AccountCost> = data
        .account_service_costs
        .into_iter()
        .filter_map(|(account_id, service_costs)| {
            let mut services: Vec<ServiceCost> = service_costs
                .into_iter()
                .filter_map(|(name, amount)| {
                    let amount = round_money(amount);
                    (!amount.is_zero()).then_some(ServiceCost { name, amount })
                })
                .collect();

            if services.is_empty() {
                return None;
            }

            services.sort_by(|a, b| b.amount.cmp(&a.amount).then_with(|| a.name.cmp(&b.name)));

            let amount = services.iter().map(|service| service.amount).sum();
            let name = account_names.get(&account_id).cloned();
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
            .cmp(&a.amount)
            .then_with(|| a.account_id.cmp(&b.account_id))
    });

    let organization_total = accounts.iter().map(|account| account.amount).sum();
    let unit = data.unit.unwrap_or_else(|| "USD".to_owned());

    BillingSummary {
        period,
        metric,
        unit,
        organization_total,
        accounts,
        estimated: data.estimated,
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::thread;
    use std::time::{Duration, Instant};

    use chrono::NaiveDate;

    use super::*;
    use crate::billing::{PeriodSelection, billing_period};

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("valid test date")
    }

    fn dec(value: &str) -> Decimal {
        value.parse().expect("valid test decimal")
    }

    fn period() -> BillingPeriod {
        billing_period(date(2026, 10, 28), PeriodSelection::MonthToDate).expect("valid period")
    }

    fn cost_explorer_client(address: SocketAddr) -> CostExplorerClient {
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
        CostExplorerClient::from_conf(config)
    }

    fn organizations_client(address: SocketAddr) -> OrganizationsClient {
        let config = aws_sdk_organizations::Config::builder()
            .behavior_version_latest()
            .region(Region::new("us-east-1"))
            .credentials_provider(aws_sdk_organizations::config::Credentials::new(
                "test-access-key",
                "test-secret-key",
                None,
                None,
                "test",
            ))
            .endpoint_url(format!("http://{address}"))
            .build();
        OrganizationsClient::from_conf(config)
    }

    /// Serve one JSON response per connection and return the requests that were
    /// received. Tests use one connection per page because every mock response
    /// closes its connection.
    fn serve_json(bodies: Vec<&'static str>) -> (SocketAddr, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let address = listener.local_addr().expect("read mock server address");
        listener
            .set_nonblocking(true)
            .expect("set nonblocking listener");

        let server = thread::spawn(move || {
            let total = bodies.len();
            let mut requests = Vec::with_capacity(total);
            let deadline = Instant::now() + Duration::from_secs(10);

            for body in bodies {
                // A bad fixture can leave the client with fewer pages to fetch
                // than the server expects, so time out instead of hanging CI.
                let (mut stream, _) = loop {
                    match listener.accept() {
                        Ok(pair) => break pair,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            assert!(
                                Instant::now() < deadline,
                                "mock server timed out after {} of {total} requests",
                                requests.len()
                            );
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => panic!("accept request: {error}"),
                    }
                };
                stream.set_nonblocking(false).expect("set blocking stream");
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .expect("set read timeout");

                requests.push(read_http_request(&mut stream));

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
            }

            requests
        });

        (address, server)
    }

    #[tokio::test]
    async fn fetches_and_aggregates_a_cost_explorer_response() {
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
        let (address, server) = serve_json(vec![body]);
        let client = cost_explorer_client(address);

        let account_names = HashMap::from([
            ("123456789012".to_owned(), "Production".to_owned()),
            ("999999999999".to_owned(), "Sandbox".to_owned()),
        ]);
        let data = fetch_costs(&client, period(), Metric::Unblended)
            .await
            .expect("fetch costs");
        let summary = assemble_summary(period(), Metric::Unblended, data, &account_names);

        let requests = server.join().expect("mock server thread panicked");
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert!(
            request.contains("AWSInsightsIndexService.GetCostAndUsage"),
            "unexpected request: {request}"
        );
        assert!(request.contains("LINKED_ACCOUNT"), "request: {request}");
        assert!(request.contains("SERVICE"), "request: {request}");
        assert!(request.contains("UnblendedCost"), "request: {request}");
        assert!(!request.contains("\"Filter\""), "request: {request}");
        assert!(request.contains("2026-10-01"), "request: {request}");
        assert!(request.contains("2026-11-01"), "request: {request}");

        assert_eq!(summary.unit, "USD");
        assert_eq!(summary.metric, Metric::Unblended);
        assert!(summary.estimated);
        assert_eq!(summary.organization_total, dec("17.75"));

        assert_eq!(summary.accounts.len(), 2);
        assert_eq!(summary.accounts[0].account_id, "123456789012");
        assert_eq!(summary.accounts[0].name.as_deref(), Some("Production"));
        assert_eq!(summary.accounts[0].amount, dec("12.75"));
        assert_eq!(summary.accounts[0].services.len(), 2);
        assert_eq!(
            summary.accounts[0].services[0].name,
            "Amazon Elastic Compute Cloud - Compute"
        );
        assert_eq!(summary.accounts[0].services[0].amount, dec("10.50"));
        assert_eq!(
            summary.accounts[0].services[1].name,
            "Amazon Simple Storage Service"
        );
        assert_eq!(summary.accounts[0].services[1].amount, dec("2.25"));

        assert_eq!(summary.accounts[1].account_id, "999999999999");
        assert_eq!(summary.accounts[1].name.as_deref(), Some("Sandbox"));
        assert_eq!(summary.accounts[1].amount, dec("5.00"));
        assert_eq!(summary.accounts[1].services.len(), 1);
    }

    #[tokio::test]
    async fn follows_cost_explorer_pagination() {
        let first_page = r#"{
            "NextPageToken": "page-two",
            "ResultsByTime": [{
                "Estimated": false,
                "Groups": [
                    {
                        "Keys": ["111111111111", "Amazon S3"],
                        "Metrics": {"UnblendedCost": {"Amount": "1.00", "Unit": "USD"}}
                    }
                ],
                "TimePeriod": {"Start": "2026-10-01", "End": "2026-11-01"},
                "Total": {}
            }]
        }"#;
        let second_page = r#"{
            "ResultsByTime": [{
                "Estimated": false,
                "Groups": [
                    {
                        "Keys": ["111111111111", "Amazon S3"],
                        "Metrics": {"UnblendedCost": {"Amount": "2.00", "Unit": "USD"}}
                    }
                ],
                "TimePeriod": {"Start": "2026-10-01", "End": "2026-11-01"},
                "Total": {}
            }]
        }"#;
        let (address, server) = serve_json(vec![first_page, second_page]);
        let client = cost_explorer_client(address);

        let data = fetch_costs(&client, period(), Metric::Unblended)
            .await
            .expect("fetch paginated costs");
        let summary = assemble_summary(period(), Metric::Unblended, data, &HashMap::new());

        let requests = server.join().expect("mock server thread panicked");
        assert_eq!(requests.len(), 2);
        assert!(
            requests[1].contains("page-two"),
            "second request: {}",
            requests[1]
        );
        assert_eq!(summary.organization_total, dec("3.00"));
        assert_eq!(summary.accounts[0].services[0].amount, dec("3.00"));
    }

    #[tokio::test]
    async fn rejects_mixed_units() {
        let body = r#"{
            "ResultsByTime": [{
                "Estimated": false,
                "Groups": [
                    {
                        "Keys": ["111111111111", "Amazon S3"],
                        "Metrics": {"UnblendedCost": {"Amount": "1.00", "Unit": "USD"}}
                    },
                    {
                        "Keys": ["111111111111", "Amazon EC2"],
                        "Metrics": {"UnblendedCost": {"Amount": "1.00", "Unit": "EUR"}}
                    }
                ],
                "TimePeriod": {"Start": "2026-10-01", "End": "2026-11-01"},
                "Total": {}
            }]
        }"#;
        let (address, server) = serve_json(vec![body]);
        let client = cost_explorer_client(address);

        let error = fetch_costs(&client, period(), Metric::Unblended)
            .await
            .expect_err("mixed units should fail");
        server.join().expect("mock server thread panicked");

        assert!(
            format!("{error:#}").contains("mixed units"),
            "unexpected error: {error:#}"
        );
    }

    #[tokio::test]
    async fn follows_organizations_pagination() {
        // Organizations uses `NextToken`; only Cost Explorer uses `NextPageToken`.
        let first_page = r#"{
            "Accounts": [{"Id": "111111111111", "Name": "Production", "Status": "ACTIVE"}],
            "NextToken": "next-accounts"
        }"#;
        let second_page = r#"{
            "Accounts": [{"Id": "222222222222", "Name": "Sandbox", "Status": "ACTIVE"}]
        }"#;
        let (address, server) = serve_json(vec![first_page, second_page]);
        let client = organizations_client(address);

        let names = fetch_account_names(&client)
            .await
            .expect("fetch account names");

        let requests = server.join().expect("mock server thread panicked");
        assert_eq!(requests.len(), 2);
        assert!(
            requests[0].contains("ListAccounts"),
            "first request: {}",
            requests[0]
        );
        assert!(
            requests[1].contains("next-accounts"),
            "second request: {}",
            requests[1]
        );
        assert_eq!(
            names.get("111111111111").map(String::as_str),
            Some("Production")
        );
        assert_eq!(
            names.get("222222222222").map(String::as_str),
            Some("Sandbox")
        );
    }

    #[test]
    fn sub_cent_services_are_filtered_and_totals_match_displayed_rows() {
        let data = CostData {
            unit: Some("USD".to_owned()),
            estimated: false,
            account_service_costs: HashMap::from([
                (
                    "111111111111".to_owned(),
                    HashMap::from([
                        ("Tiny A".to_owned(), dec("0.004")),
                        ("Tiny B".to_owned(), dec("0.004")),
                    ]),
                ),
                (
                    "222222222222".to_owned(),
                    HashMap::from([("Visible".to_owned(), dec("0.006"))]),
                ),
            ]),
        };

        let summary = assemble_summary(period(), Metric::NetUnblended, data, &HashMap::new());
        let displayed_total: Decimal = summary
            .accounts
            .iter()
            .flat_map(|account| account.services.iter())
            .map(|service| service.amount)
            .sum();

        assert_eq!(summary.accounts.len(), 1);
        assert_eq!(summary.accounts[0].account_id, "222222222222");
        assert_eq!(summary.accounts[0].amount, dec("0.01"));
        assert_eq!(summary.organization_total, dec("0.01"));
        assert_eq!(displayed_total, summary.organization_total);
    }

    #[test]
    fn account_totals_are_the_sum_of_their_services() {
        let data = CostData {
            unit: Some("USD".to_owned()),
            estimated: true,
            account_service_costs: HashMap::from([(
                "111111111111".to_owned(),
                HashMap::from([
                    ("Amazon EC2".to_owned(), dec("10.00")),
                    ("AWS Lambda".to_owned(), dec("-2.50")),
                ]),
            )]),
        };

        let summary = assemble_summary(period(), Metric::Unblended, data, &HashMap::new());

        assert_eq!(summary.accounts[0].amount, dec("7.50"));
        assert_eq!(summary.organization_total, dec("7.50"));
        assert_eq!(summary.accounts[0].services.len(), 2);
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
