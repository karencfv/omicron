// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::project::SERVICES_DB_NAME;
use nexus_db_model as model;
use nexus_types::external_api::params;
use omicron_common::address::SERVICE_VPC_IPV6_PREFIX;
use omicron_common::api::external::IdentityMetadataCreateParams;
use std::sync::LazyLock;

/// UUID of built-in VPC for internal services on the rack.
pub static SERVICES_VPC_ID: LazyLock<uuid::Uuid> = LazyLock::new(|| {
    "001de000-074c-4000-8000-000000000000"
        .parse()
        .expect("invalid uuid for builtin services vpc id")
});

/// UUID of VpcRouter for built-in Services VPC.
pub static SERVICES_VPC_ROUTER_ID: LazyLock<uuid::Uuid> = LazyLock::new(|| {
    "001de000-074c-4000-8000-000000000001"
        .parse()
        .expect("invalid uuid for builtin services vpc router id")
});

/// UUID of InternetGateway for built-in Services VPC.
pub static SERVICES_INTERNET_GATEWAY_ID: LazyLock<uuid::Uuid> =
    LazyLock::new(|| {
        "001de000-074c-4000-8000-000000000002"
            .parse()
            .expect("invalid uuid for builtin services internet gateway id")
    });

/// UUID of InternetGateway IPv4 default route for built-in Services VPC.
pub static SERVICES_INTERNET_GATEWAY_DEFAULT_ROUTE_V4: LazyLock<uuid::Uuid> =
    LazyLock::new(|| {
        "001de000-074c-4000-8000-000000000003"
        .parse()
        .expect("invalid uuid for builtin services internet gateway default route v4")
    });

/// UUID of InternetGateway IPv6 default route for built-in Services VPC.
pub static SERVICES_INTERNET_GATEWAY_DEFAULT_ROUTE_V6: LazyLock<uuid::Uuid> =
    LazyLock::new(|| {
        "001de000-074c-4000-8000-000000000004"
        .parse()
        .expect("invalid uuid for builtin services internet gateway default route v4")
    });

/// Built-in VPC for internal services on the rack.
pub static SERVICES_VPC: LazyLock<model::IncompleteVpc> = LazyLock::new(|| {
    model::IncompleteVpc::new(
        *SERVICES_VPC_ID,
        *super::project::SERVICES_PROJECT_ID,
        *SERVICES_VPC_ROUTER_ID,
        params::VpcCreate {
            identity: IdentityMetadataCreateParams {
                name: SERVICES_DB_NAME.parse().unwrap(),
                description: "Built-in VPC for Oxide Services".to_string(),
            },
            ipv6_prefix: Some(*SERVICE_VPC_IPV6_PREFIX),
            dns_name: SERVICES_DB_NAME.parse().unwrap(),
        },
    )
    // `IncompleteVpc::new` only fails if given an invalid `ipv6_prefix`
    // but we know `SERVICE_VPC_IPV6_PREFIX` is valid.
    .unwrap()
});
