// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Helpers for configuring and starting the bootstore during bootstrap agent
//! startup.

#![allow(clippy::result_large_err)]

use super::config::BOOTSTORE_PORT;
use super::server::StartError;
use bootstore::schemes::v0 as bootstore;
use camino::Utf8PathBuf;
use omicron_ddm_admin_client::Client as DdmAdminClient;
use sled_hardware_types::underlay::BootstrapInterface;
use sled_hardware_types::Baseboard;
use sled_storage::dataset::CLUSTER_DATASET;
use sled_storage::resources::AllDisks;
use slog::Logger;
use std::collections::BTreeSet;
use std::net::Ipv6Addr;
use std::net::SocketAddrV6;
use std::time::Duration;

const BOOTSTORE_FSM_STATE_FILE: &str = "bootstore-fsm-state.json";
const BOOTSTORE_NETWORK_CONFIG_FILE: &str = "bootstore-network-config.json";

pub fn new_bootstore_config(
    all_disks: &AllDisks,
    baseboard: Baseboard,
    global_zone_bootstrap_ip: Ipv6Addr,
) -> Result<bootstore::Config, StartError> {
    Ok(bootstore::Config {
        id: baseboard,
        addr: SocketAddrV6::new(global_zone_bootstrap_ip, BOOTSTORE_PORT, 0, 0),
        time_per_tick: Duration::from_millis(250),
        learn_timeout: Duration::from_secs(5),
        rack_init_timeout: Duration::from_secs(300),
        rack_secret_request_timeout: Duration::from_secs(5),
        fsm_state_ledger_paths: bootstore_fsm_state_paths(&all_disks)?,
        network_config_ledger_paths: bootstore_network_config_paths(
            &all_disks,
        )?,
    })
}

fn bootstore_fsm_state_paths(
    all_disks: &AllDisks,
) -> Result<Vec<Utf8PathBuf>, StartError> {
    let paths: Vec<_> = all_disks
        .all_m2_mountpoints(CLUSTER_DATASET)
        .into_iter()
        .map(|p| p.join(BOOTSTORE_FSM_STATE_FILE))
        .collect();

    if paths.is_empty() {
        return Err(StartError::MissingM2Paths(CLUSTER_DATASET));
    }
    Ok(paths)
}

fn bootstore_network_config_paths(
    all_disks: &AllDisks,
) -> Result<Vec<Utf8PathBuf>, StartError> {
    let paths: Vec<_> = all_disks
        .all_m2_mountpoints(CLUSTER_DATASET)
        .into_iter()
        .map(|p| p.join(BOOTSTORE_NETWORK_CONFIG_FILE))
        .collect();

    if paths.is_empty() {
        return Err(StartError::MissingM2Paths(CLUSTER_DATASET));
    }
    Ok(paths)
}

pub async fn poll_ddmd_for_bootstore_peer_update(
    log: Logger,
    bootstore_node_handle: bootstore::NodeHandle,
) {
    let mut current_peers: BTreeSet<SocketAddrV6> = BTreeSet::new();
    // We're talking to a service's admin interface on localhost and
    // we're only asking for its current state. We use a retry in a loop
    // instead of `backoff`.
    //
    // We also use this timeout in the case of spurious ddmd failures
    // that require a reconnection from the ddmd_client.
    const RETRY: tokio::time::Duration = tokio::time::Duration::from_secs(5);
    let ddmd_client = DdmAdminClient::localhost(&log).unwrap();
    loop {
        match ddmd_client
            .derive_bootstrap_addrs_from_prefixes(&[
                BootstrapInterface::GlobalZone,
            ])
            .await
        {
            Ok(addrs) => {
                let peers: BTreeSet<_> = addrs
                    .map(|ip| SocketAddrV6::new(ip, BOOTSTORE_PORT, 0, 0))
                    .collect();
                info!(
                    log,
                    "DEBUG POLL DDMD FOR BOOTSTORE: peers: {:?}, current peers: {:?}",
                    peers,
                    current_peers
                );
                if peers != current_peers {
                    info!(
                        log,
                        "DEBUG POLL DDMD FOR BOOTSTORE: peers {:?} not equal to current peers {:?}. Setting current_peers = peers",
                        peers,
                        current_peers
                    );
                    current_peers = peers;
                    if let Err(e) = bootstore_node_handle
                        .load_peer_addresses(current_peers.clone())
                        .await
                    {
                        error!(
                            log,
                            concat!(
                                "DEBUG POLL DDMD FOR BOOTSTORE: Bootstore comms error: {}. ",
                                "bootstore::Node task must have panicked",
                            ),
                            e
                        );
                        return;
                    }
                }
            }
            Err(err) => {
                warn!(
                    log, "DEBUG POLL DDMD FOR BOOTSTORE: Failed to get prefixes from ddmd";
                    "err" => #%err,
                );
                break;
            }
        }
        tokio::time::sleep(RETRY).await;
    }
}
