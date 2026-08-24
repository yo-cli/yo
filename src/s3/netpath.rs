// Which network path this host's S3 uploads actually take — detected, never
// asked. The user should not have to know their VPC topology to get a correct
// bill estimate.
//
// Why this is worth two API calls: a NAT gateway charges $0.045 per GB
// processed, on the very bytes this tool uploads. That is the single largest
// per-byte rate in the whole tool — more than CRR ($0.02) or Transfer
// Acceleration ($0.04) — and on the invoice it hides under "Data Processed by
// NAT Gateways" rather than any S3 line item. Not counting it lets a run burn
// past its budget; counting it when an S3 gateway endpoint is actually
// carrying the traffic for free makes the run stop early. Guessing is wrong
// in both directions, so we look.

use aws_config::SdkConfig;
use aws_sdk_ec2::types::{Filter, RouteTable, VpcEndpointType};
use colored::Colorize;
use std::time::Duration;

/// IMDS is unreachable off EC2; cap the wait so laptops do not stall.
const IMDS_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressPath {
    /// Not an EC2 instance (laptop / on-prem): no VPC processing charges.
    NotOnEc2,
    /// An S3 gateway endpoint carries this subnet's S3 traffic — free path.
    GatewayEndpoint,
    /// Private subnet with a default route at a NAT gateway: $0.045/GB.
    NatGateway,
    /// Public subnet reaching S3 through an internet gateway: free in-region.
    InternetGateway,
    /// Undetermined: IMDS answered but the EC2 lookups did not.
    Unknown,
}

impl EgressPath {
    /// Does this path add a per-byte data-processing fee to every upload?
    pub fn bills_data_processing(self) -> bool {
        matches!(self, EgressPath::NatGateway)
    }

    /// One line for the preflight output. Silent (None) when there is nothing
    /// worth saying — no path fee is possible and the user needs no nudge.
    pub fn describe(self) -> Option<String> {
        match self {
            EgressPath::NotOnEc2 => None,
            EgressPath::InternetGateway => None,
            EgressPath::GatewayEndpoint => Some(format!(
                "{} S3 走 Gateway Endpoint(免费路径),无 NAT 流量费",
                "✓".green()
            )),
            EgressPath::NatGateway => Some(format!(
                "{} 检测到 S3 流量经 NAT 网关 —— {}已计入预算。\
                 想省掉这笔钱:为该子网加一个免费的 S3 Gateway Endpoint",
                "⚠".yellow().bold(),
                "每字节额外 $0.045/GB,".bold()
            )),
            EgressPath::Unknown => Some(format!(
                "{} 无法确认 S3 流量是否经 NAT 网关(缺 ec2:DescribeRouteTables / \
                 DescribeVpcEndpoints 权限)。若实际走 NAT,账单会比预估多 $0.045/GB",
                "ℹ".blue()
            )),
        }
    }
}

/// Work out how this host reaches S3. Never fails: anything undeterminable
/// degrades to `NotOnEc2` / `Unknown`, which add no fee.
///
/// `bucket_region` matters: an S3 gateway endpoint only carries traffic to S3
/// in its OWN region, so a cross-region bucket still goes out via NAT even
/// when an endpoint exists.
pub async fn detect(shared: &SdkConfig, bucket_region: Option<&str>) -> EgressPath {
    let Some(net) = instance_network().await else {
        return EgressPath::NotOnEc2;
    };

    let ec2 = aws_sdk_ec2::Client::new(shared);

    let Some(route_table) = route_table_for(&ec2, &net).await else {
        return EgressPath::Unknown;
    };

    // A gateway endpoint takes S3 traffic off the NAT path — but only for
    // buckets in its own region.
    let same_region = bucket_region.is_none_or(|b| b == net.region);
    if same_region {
        match s3_gateway_endpoint(&ec2, &net, &route_table).await {
            Some(true) => return EgressPath::GatewayEndpoint,
            Some(false) => {}
            // Cannot tell whether the free path exists; do not guess a fee.
            None => return EgressPath::Unknown,
        }
    }

    classify_default_route(&route_table)
}

/// Only the default route matters: S3's public endpoints are not in the VPC
/// CIDR, so whatever serves 0.0.0.0/0 is what carries the upload.
fn classify_default_route(route_table: &RouteTable) -> EgressPath {
    for route in route_table.routes() {
        if route.destination_cidr_block() != Some("0.0.0.0/0") {
            continue;
        }
        if route.nat_gateway_id().is_some() {
            return EgressPath::NatGateway;
        }
        if route.gateway_id().is_some_and(|g| g.starts_with("igw-")) {
            return EgressPath::InternetGateway;
        }
    }
    EgressPath::Unknown
}

struct InstanceNetwork {
    subnet_id: String,
    vpc_id: String,
    region: String,
}

/// Ask IMDS which subnet/VPC/region we are in. None = not on EC2.
async fn instance_network() -> Option<InstanceNetwork> {
    let lookup = async {
        let imds = aws_config::imds::Client::builder().build();
        let mac = imds.get("/latest/meta-data/mac").await.ok()?;
        let mac = mac.as_ref().trim().to_string();
        let base = format!("/latest/meta-data/network/interfaces/macs/{}", mac);
        let subnet_id = imds.get(format!("{}/subnet-id", base)).await.ok()?;
        let vpc_id = imds.get(format!("{}/vpc-id", base)).await.ok()?;
        let region = imds
            .get("/latest/meta-data/placement/region")
            .await
            .ok()?;
        Some(InstanceNetwork {
            subnet_id: subnet_id.as_ref().trim().to_string(),
            vpc_id: vpc_id.as_ref().trim().to_string(),
            region: region.as_ref().trim().to_string(),
        })
    };
    tokio::time::timeout(IMDS_TIMEOUT, lookup).await.ok().flatten()
}

/// The route table governing this subnet: its explicit association, or the
/// VPC main table when the subnet has none.
async fn route_table_for(
    ec2: &aws_sdk_ec2::Client,
    net: &InstanceNetwork,
) -> Option<RouteTable> {
    let explicit = ec2
        .describe_route_tables()
        .filters(
            Filter::builder()
                .name("association.subnet-id")
                .values(&net.subnet_id)
                .build(),
        )
        .send()
        .await
        .ok()?;
    if let Some(rt) = explicit.route_tables().first() {
        return Some(rt.clone());
    }

    let main = ec2
        .describe_route_tables()
        .filters(Filter::builder().name("vpc-id").values(&net.vpc_id).build())
        .filters(Filter::builder().name("association.main").values("true").build())
        .send()
        .await
        .ok()?;
    main.route_tables().first().cloned()
}

/// Is an S3 gateway endpoint attached to this route table?
/// None = the lookup failed, which is not the same as "no endpoint".
///
/// The filters make the answer precise server-side: only Gateway-type
/// endpoints for this region's S3 service in this VPC come back, so our route
/// table appearing among their `routeTableIdSet` means S3 traffic takes the
/// free path.
async fn s3_gateway_endpoint(
    ec2: &aws_sdk_ec2::Client,
    net: &InstanceNetwork,
    route_table: &RouteTable,
) -> Option<bool> {
    let Some(route_table_id) = route_table.route_table_id() else {
        return Some(false);
    };
    let out = ec2
        .describe_vpc_endpoints()
        .filters(Filter::builder().name("vpc-id").values(&net.vpc_id).build())
        .filters(
            Filter::builder()
                .name("service-name")
                .values(format!("com.amazonaws.{}.s3", net.region))
                .build(),
        )
        .send()
        .await
        .ok()?;
    Some(out.vpc_endpoints().iter().any(|ep| {
        ep.vpc_endpoint_type() == Some(&VpcEndpointType::Gateway)
            && ep.route_table_ids().iter().any(|id| id == route_table_id)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_ec2::types::Route;

    fn table(routes: Vec<Route>) -> RouteTable {
        RouteTable::builder()
            .route_table_id("rtb-1")
            .set_routes(Some(routes))
            .build()
    }

    fn route(cidr: &str, nat: Option<&str>, gateway: Option<&str>) -> Route {
        let mut builder = Route::builder().destination_cidr_block(cidr);
        if let Some(nat) = nat {
            builder = builder.nat_gateway_id(nat);
        }
        if let Some(gateway) = gateway {
            builder = builder.gateway_id(gateway);
        }
        builder.build()
    }

    #[test]
    fn nat_on_the_default_route_is_billed() {
        let rt = table(vec![route("0.0.0.0/0", Some("nat-1"), None)]);
        assert_eq!(classify_default_route(&rt), EgressPath::NatGateway);
        assert!(EgressPath::NatGateway.bills_data_processing());
    }

    #[test]
    fn internet_gateway_is_free() {
        let rt = table(vec![route("0.0.0.0/0", None, Some("igw-1"))]);
        assert_eq!(classify_default_route(&rt), EgressPath::InternetGateway);
        assert!(!EgressPath::InternetGateway.bills_data_processing());
    }

    /// A NAT route for a narrow CIDR does not carry S3 traffic — S3's public
    /// endpoints only match the default route.
    #[test]
    fn nat_on_a_narrow_route_is_not_the_s3_path() {
        let rt = table(vec![
            route("10.1.0.0/16", Some("nat-1"), None),
            route("0.0.0.0/0", None, Some("igw-1")),
        ]);
        assert_eq!(classify_default_route(&rt), EgressPath::InternetGateway);
    }

    #[test]
    fn local_only_table_is_undetermined() {
        let rt = table(vec![route("10.0.0.0/16", None, Some("local"))]);
        assert_eq!(classify_default_route(&rt), EgressPath::Unknown);
    }

    #[test]
    fn only_nat_adds_a_fee() {
        for path in [
            EgressPath::NotOnEc2,
            EgressPath::GatewayEndpoint,
            EgressPath::InternetGateway,
            EgressPath::Unknown,
        ] {
            assert!(!path.bills_data_processing(), "{:?}", path);
        }
    }

    /// Paths that cannot cost anything stay quiet; the rest explain themselves.
    #[test]
    fn only_actionable_paths_print() {
        assert!(EgressPath::NotOnEc2.describe().is_none());
        assert!(EgressPath::InternetGateway.describe().is_none());
        assert!(EgressPath::NatGateway.describe().is_some());
        assert!(EgressPath::GatewayEndpoint.describe().is_some());
        assert!(EgressPath::Unknown.describe().is_some());
    }
}
