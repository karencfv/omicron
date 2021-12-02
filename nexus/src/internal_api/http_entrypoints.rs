// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::nexus::BASE_ARTIFACT_DIR;
/**
 * Handler functions (entrypoints) for HTTP APIs internal to the control plane
 */
use crate::ServerContext;

use super::params::{
    DatasetPutRequest, DatasetPutResponse, OximeterInfo, SledAgentStartupInfo,
    ZpoolPutRequest, ZpoolPutResponse,
};
use dropshot::endpoint;
use dropshot::ApiDescription;
use dropshot::HttpError;
use dropshot::HttpResponseOk;
use dropshot::HttpResponseUpdatedNoContent;
use dropshot::Path;
use dropshot::RequestContext;
use dropshot::TypedBody;
use http::{Response, StatusCode};
use hyper::Body;
use omicron_common::api::internal::nexus::DiskRuntimeState;
use omicron_common::api::internal::nexus::InstanceRuntimeState;
use omicron_common::api::internal::nexus::ProducerEndpoint;
use oximeter::types::ProducerResults;
use oximeter_producer::{collect, ProducerIdPathParams};
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

type NexusApiDescription = ApiDescription<Arc<ServerContext>>;

/**
 * Returns a description of the internal nexus API
 */
pub fn internal_api() -> NexusApiDescription {
    fn register_endpoints(api: &mut NexusApiDescription) -> Result<(), String> {
        api.register(cpapi_sled_agents_post)?;
        api.register(zpool_put)?;
        api.register(dataset_put)?;
        api.register(cpapi_instances_put)?;
        api.register(cpapi_disks_put)?;
        api.register(cpapi_producers_post)?;
        api.register(cpapi_collectors_post)?;
        api.register(cpapi_metrics_collect)?;
        api.register(cpapi_artifact_download)?;
        Ok(())
    }

    let mut api = NexusApiDescription::new();
    if let Err(err) = register_endpoints(&mut api) {
        panic!("failed to register entrypoints: {}", err);
    }
    api
}

/**
 * Path parameters for Sled Agent requests (internal API)
 */
#[derive(Deserialize, JsonSchema)]
struct SledAgentPathParam {
    sled_id: Uuid,
}

/// Report that the sled agent for the specified sled has come online.
// TODO: Should probably be "PUT", since:
// 1. We're upserting the value
// 2. The client supplies the UUID
// 3. This call is idempotent (mod "time_modified").
#[endpoint {
     method = POST,
     path = "/sled_agents/{sled_id}",
 }]
async fn cpapi_sled_agents_post(
    rqctx: Arc<RequestContext<Arc<ServerContext>>>,
    path_params: Path<SledAgentPathParam>,
    sled_info: TypedBody<SledAgentStartupInfo>,
) -> Result<HttpResponseUpdatedNoContent, HttpError> {
    let apictx = rqctx.context();
    let nexus = &apictx.nexus;
    let path = path_params.into_inner();
    let si = sled_info.into_inner();
    let sled_id = &path.sled_id;
    let handler = async {
        nexus.upsert_sled(*sled_id, si.sa_address).await?;
        Ok(HttpResponseUpdatedNoContent())
    };
    apictx.internal_latencies.instrument_dropshot_handler(&rqctx, handler).await
}

/**
 * Path parameters for Sled Agent requests (internal API)
 */
#[derive(Deserialize, JsonSchema)]
struct ZpoolPathParam {
    sled_id: Uuid,
    zpool_id: Uuid,
}

/**
 * Report that a pool for a specified sled has come online.
 */
#[endpoint {
     method = PUT,
     path = "/sled_agents/{sled_id}/zpools/{zpool_id}",
 }]
async fn zpool_put(
    rqctx: Arc<RequestContext<Arc<ServerContext>>>,
    path_params: Path<ZpoolPathParam>,
    pool_info: TypedBody<ZpoolPutRequest>,
) -> Result<HttpResponseOk<ZpoolPutResponse>, HttpError> {
    let apictx = rqctx.context();
    let nexus = &apictx.nexus;
    let path = path_params.into_inner();
    let pi = pool_info.into_inner();
    nexus.upsert_zpool(path.zpool_id, path.sled_id, pi).await?;
    Ok(HttpResponseOk(ZpoolPutResponse {}))
}

#[derive(Deserialize, JsonSchema)]
struct DatasetPathParam {
    zpool_id: Uuid,
    dataset_id: Uuid,
}

/**
 * Report that a dataset within a pool has come online.
 */
#[endpoint {
     method = PUT,
     path = "/zpools/{zpool_id}/dataset/{dataset_id}",
 }]
async fn dataset_put(
    rqctx: Arc<RequestContext<Arc<ServerContext>>>,
    path_params: Path<DatasetPathParam>,
    info: TypedBody<DatasetPutRequest>,
) -> Result<HttpResponseOk<DatasetPutResponse>, HttpError> {
    let apictx = rqctx.context();
    let nexus = &apictx.nexus;
    let path = path_params.into_inner();
    let info = info.into_inner();
    nexus
        .upsert_dataset(
            path.dataset_id,
            path.zpool_id,
            info.address,
            info.kind.into(),
        )
        .await?;
    Ok(HttpResponseOk(DatasetPutResponse { reservation: None, quota: None }))
}

/**
 * Path parameters for Instance requests (internal API)
 */
#[derive(Deserialize, JsonSchema)]
struct InstancePathParam {
    instance_id: Uuid,
}

/**
 * Report updated state for an instance.
 */
#[endpoint {
     method = PUT,
     path = "/instances/{instance_id}",
 }]
async fn cpapi_instances_put(
    rqctx: Arc<RequestContext<Arc<ServerContext>>>,
    path_params: Path<InstancePathParam>,
    new_runtime_state: TypedBody<InstanceRuntimeState>,
) -> Result<HttpResponseUpdatedNoContent, HttpError> {
    let apictx = rqctx.context();
    let nexus = &apictx.nexus;
    let path = path_params.into_inner();
    let new_state = new_runtime_state.into_inner();
    let handler = async {
        nexus.notify_instance_updated(&path.instance_id, &new_state).await?;
        Ok(HttpResponseUpdatedNoContent())
    };
    apictx.internal_latencies.instrument_dropshot_handler(&rqctx, handler).await
}

/**
 * Path parameters for Disk requests (internal API)
 */
#[derive(Deserialize, JsonSchema)]
struct DiskPathParam {
    disk_id: Uuid,
}

/**
 * Report updated state for a disk.
 */
#[endpoint {
     method = PUT,
     path = "/disks/{disk_id}",
 }]
async fn cpapi_disks_put(
    rqctx: Arc<RequestContext<Arc<ServerContext>>>,
    path_params: Path<DiskPathParam>,
    new_runtime_state: TypedBody<DiskRuntimeState>,
) -> Result<HttpResponseUpdatedNoContent, HttpError> {
    let apictx = rqctx.context();
    let nexus = &apictx.nexus;
    let path = path_params.into_inner();
    let new_state = new_runtime_state.into_inner();
    let handler = async {
        nexus.notify_disk_updated(&path.disk_id, &new_state).await?;
        Ok(HttpResponseUpdatedNoContent())
    };
    apictx.internal_latencies.instrument_dropshot_handler(&rqctx, handler).await
}

/**
 * Accept a registration from a new metric producer
 */
#[endpoint {
     method = POST,
     path = "/metrics/producers",
 }]
async fn cpapi_producers_post(
    request_context: Arc<RequestContext<Arc<ServerContext>>>,
    producer_info: TypedBody<ProducerEndpoint>,
) -> Result<HttpResponseUpdatedNoContent, HttpError> {
    let context = request_context.context();
    let nexus = &context.nexus;
    let producer_info = producer_info.into_inner();
    let handler = async {
        nexus.assign_producer(producer_info).await?;
        Ok(HttpResponseUpdatedNoContent())
    };
    context
        .internal_latencies
        .instrument_dropshot_handler(&request_context, handler)
        .await
}

/**
 * Accept a notification of a new oximeter collection server.
 */
#[endpoint {
     method = POST,
     path = "/metrics/collectors",
 }]
async fn cpapi_collectors_post(
    request_context: Arc<RequestContext<Arc<ServerContext>>>,
    oximeter_info: TypedBody<OximeterInfo>,
) -> Result<HttpResponseUpdatedNoContent, HttpError> {
    let context = request_context.context();
    let nexus = &context.nexus;
    let oximeter_info = oximeter_info.into_inner();
    let handler = async {
        nexus.upsert_oximeter_collector(&oximeter_info).await?;
        Ok(HttpResponseUpdatedNoContent())
    };
    context
        .internal_latencies
        .instrument_dropshot_handler(&request_context, handler)
        .await
}

/**
 * Endpoint for oximeter to collect nexus server metrics.
 */
#[endpoint {
    method = GET,
    path = "/metrics/collect/{producer_id}",
}]
async fn cpapi_metrics_collect(
    request_context: Arc<RequestContext<Arc<ServerContext>>>,
    path_params: Path<ProducerIdPathParams>,
) -> Result<HttpResponseOk<ProducerResults>, HttpError> {
    let context = request_context.context();
    let producer_id = path_params.into_inner().producer_id;
    let handler =
        async { collect(&context.producer_registry, producer_id).await };
    context
        .internal_latencies
        .instrument_dropshot_handler(&request_context, handler)
        .await
}

#[derive(Deserialize, JsonSchema)]
struct AllPath {
    path: String,
}

/// Endpoint used by Sled Agents to download cached artifacts.
#[endpoint {
    method = GET,
    path = "/artifacts/{path}",
}]
async fn cpapi_artifact_download(
    request_context: Arc<RequestContext<Arc<ServerContext>>>,
    path: Path<AllPath>,
) -> Result<Response<Body>, HttpError> {
    let context = request_context.context();
    let nexus = &context.nexus;
    let mut entry = PathBuf::from(BASE_ARTIFACT_DIR);

    // TODO: Most of the below code is ready to accept a multi-component path,
    // such as in:
    // https://github.com/oxidecomputer/dropshot/blob/78be3deda556a9339ea09f3a9961fd91389f8757/dropshot/examples/file_server.rs#L86-L89
    //
    // However, openapi does *not* like that currently, so we limit the endpoint
    // to only accepting single-component paths.
    let path = vec![path.into_inner().path];

    for component in &path {
        // Dropshot should not provide "." and ".." components.
        assert_ne!(component, ".");
        assert_ne!(component, "..");
        entry.push(component);

        if entry.exists() {
            // We explicitly prohibit consumers from following symlinks to prevent
            // showing data outside of the intended directory.
            let m = entry.symlink_metadata().map_err(|e| {
                HttpError::for_bad_request(
                    None,
                    format!("Failed to query file metadata: {}", e),
                )
            })?;
            if m.file_type().is_symlink() {
                return Err(HttpError::for_bad_request(
                    None,
                    "Cannot traverse symlinks".to_string(),
                ));
            }
        }
    }

    // Note - at this point, "entry" may or may not actually exist.
    // We try to avoid creating intermediate artifacts until we know there
    // is something "real" to download, as this would let malformed paths
    // create defunct intermediate directories.
    if entry.is_dir() {
        return Err(HttpError::for_bad_request(
            None,
            "Directory download not supported".to_string(),
        ));
    }
    let body = nexus.download_artifact(&entry).await?;

    // Derive the MIME type from the file name
    let content_type = mime_guess::from_path(&entry)
        .first()
        .map_or_else(|| "text/plain".to_string(), |m| m.to_string());

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(http::header::CONTENT_TYPE, content_type)
        .body(body.into())?)
}
