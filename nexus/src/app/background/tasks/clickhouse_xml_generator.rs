// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Background task for generating ClickHouse server and keeper XML configuration files.
//!
//! blah blah blah more information

use anyhow::Context;
use camino::Utf8PathBuf;
use clickhouse_admin_client::types::ServerConfigurableSettings;
use clickhouse_admin_types::{
    config::ClickhouseHost, ServerId, ServerSettings,
};
use nexus_auth::context::OpContext;
use omicron_common::api::external::Generation;
// use omicron_common::address::CLICKHOUSE_ADMIN_PORT;
use std::{
    net::{Ipv6Addr, SocketAddrV6},
    str::FromStr,
};

#[allow(dead_code)]
async fn generate_clickhose_server_config(
    opctx: &OpContext,
    // TODO: get the datastore like we get it in the SMF service.
    // "String" is not the correct type that is going to be used
    // this is for development purposes only
    datastore: String,
    admin_addr: SocketAddrV6,
    _server_addrs: Vec<SocketAddrV6>,
    _keeper_addrs: Vec<SocketAddrV6>,
) -> anyhow::Result<()> {
    let admin_url = format!("http://{admin_addr}");
    let admin_client =
        clickhouse_admin_client::Client::new(&admin_url, opctx.log.clone());
    // TODO: Do something with this variable?
    let _config = admin_client
        .generate_server_config(&ServerConfigurableSettings {
            // TODO: Retrieve generation number
            generation: Generation::from_u32(1),
            settings: ServerSettings {
                // TODO: Can config dir can come from a const?
                config_dir: Utf8PathBuf::new(),
                // TODO: This server ID will be saved somewhere
                id: ServerId(1),
                listen_addr: *admin_addr.ip(),
                datastore_path: Utf8PathBuf::from_str(&datastore).unwrap(),
                // TODO: Get information about the other keepers and replicas
                keepers: vec![ClickhouseHost::Ipv6(
                    Ipv6Addr::from_str("::1").unwrap(),
                )],
                remote_servers: vec![ClickhouseHost::Ipv6(
                    Ipv6Addr::from_str("::1").unwrap(),
                )],
            },
        })
        .await
        .with_context(|| {
            // TODO: Retireve zone ID and show it here
            "failed to generate ClickHouse configuration file for 'ZONE_ID' "
        })?
        .into_inner();

    Ok(())
}
