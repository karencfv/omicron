// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Types for serving produced metric data to an Oximeter collector server.

// Copyright 2021 Oxide Computer Company

use dropshot::endpoint;
use dropshot::ApiDescription;
use dropshot::ConfigDropshot;
use dropshot::ConfigLogging;
use dropshot::HttpError;
use dropshot::HttpResponseOk;
use dropshot::HttpServer;
use dropshot::HttpServerStarter;
use dropshot::Path;
use dropshot::RequestContext;
use omicron_common::api::internal::nexus::ProducerEndpoint;
use omicron_common::FileKv;
use oximeter::types::ProducerRegistry;
use oximeter::types::ProducerResults;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use slog::debug;
use slog::error;
use slog::info;
use slog::o;
use slog::Drain;
use slog::Logger;
use std::net::SocketAddr;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Error)]
pub enum Error {
    #[error("Error running producer HTTP server: {0}")]
    Server(String),

    #[error("Error registering as metric producer: {0}")]
    RegistrationError(String),
}

/// Either configuration for building a logger, or an actual logger already
/// instantiated.
///
/// This can be used to start a [`Server`] with a new logger or a child of a
/// parent logger if desired.
#[derive(Debug, Clone)]
pub enum LogConfig {
    /// Configuration for building a new logger.
    Config(ConfigLogging),
    /// An explicit logger to use.
    Logger(Logger),
}

/// Information used to configure a [`Server`]
#[derive(Debug, Clone)]
pub struct Config {
    /// The information for contacting this server, and collecting its metrics.
    pub server_info: ProducerEndpoint,
    /// The address at which we attempt to register as a producer.
    pub registration_address: SocketAddr,
    /// Configuration for starting the Dropshot server used to produce metrics.
    pub dropshot: ConfigDropshot,
    /// The logging configuration or actual logger used to emit logs.
    pub log: LogConfig,
}

/// A Dropshot server used to expose metrics to be collected over the network.
///
/// This is a "batteries-included" HTTP server, meant to be used in applications that don't
/// otherwise run a server. The standalone functions [`register`] and [`collect`] can be used as
/// part of an existing Dropshot server's API.
pub struct Server {
    registry: ProducerRegistry,
    server: HttpServer<ProducerRegistry>,
}

impl Server {
    /// Start a new metric server, registering it with the chosen endpoint, and listening for
    /// requests on the associated address and route.
    pub async fn start(config: &Config) -> Result<Self, Error> {
        // Clone mutably, as we may update the address after the server starts, see below.
        let mut config = config.clone();

        // Build a logger, either using the configuration or actual logger
        // provided. First build the base logger from the configuration or a
        // clone of the provided logger, and then add the DTrace and Dropshot
        // loggers on top of it.
        let base_logger = match config.log {
            LogConfig::Config(conf) => conf
                .to_logger("metric-server")
                .map_err(|msg| Error::Server(msg.to_string()))?,
            LogConfig::Logger(log) => log.clone(),
        };
        let (drain, registration) = slog_dtrace::with_drain(base_logger);
        let log = Logger::root(drain.fuse(), slog::o!(FileKv));
        if let slog_dtrace::ProbeRegistration::Failed(e) = registration {
            let msg = format!("failed to register DTrace probes: {}", e);
            error!(log, "failed to register DTrace probes: {}", e);
            return Err(Error::Server(msg));
        } else {
            debug!(log, "registered DTrace probes");
        }
        let dropshot_log = log.new(o!("component" => "dropshot"));

        // Build the producer registry and server that uses it as its context.
        let registry = ProducerRegistry::with_id(config.server_info.id);
        let server = HttpServerStarter::new(
            &config.dropshot,
            metric_server_api(),
            registry.clone(),
            &dropshot_log,
        )
        .map_err(|e| Error::Server(e.to_string()))?
        .start();

        // Client code may decide to assign a specific address and/or port, or to listen on any
        // available address and port, assigned by the OS. For example, `[::1]:0` would assign any
        // port on localhost. If needed, update the address in the `ProducerEndpoint` with the
        // actual address the server has bound.
        //
        // TODO-robustness: Is there a better way to do this? We'd like to support users picking an
        // exact address or using whatever's available. The latter is useful during tests or other
        // situations in which we don't know which ports are available.
        if config.server_info.address != server.local_addr() {
            assert_eq!(config.server_info.address.port(), 0);
            debug!(
                log,
                "Requested any available port, Dropshot server has been bound to {}",
                server.local_addr(),
            );
            config.server_info.address = server.local_addr();
        }

        debug!(log, "registering metric server as a producer");
        register(config.registration_address, &log, &config.server_info)
            .await?;
        info!(
            log,
            "starting oximeter metric server";
            "route" => config.server_info.collection_route(),
            "producer_id" => ?registry.producer_id(),
            "address" => config.server_info.address,
        );
        Ok(Self { registry, server })
    }

    /// Serve requests for metrics.
    pub async fn serve_forever(self) -> Result<(), Error> {
        self.server.await.map_err(Error::Server)
    }

    /// Close the server
    pub async fn close(self) -> Result<(), Error> {
        self.server.close().await.map_err(Error::Server)
    }

    /// Return the [`ProducerRegistry`] managed by this server.
    ///
    /// The registry is thread-safe and clonable, so the returned reference can be used throughout
    /// an application to register types implementing the [`Producer`](oximeter::traits::Producer)
    /// trait. The samples generated by the registered producers will be included in response to a
    ///  request on the collection endpoint.
    pub fn registry(&self) -> &ProducerRegistry {
        &self.registry
    }

    /// Return the server's local listening address
    pub fn address(&self) -> std::net::SocketAddr {
        self.server.local_addr()
    }
}

// Register API endpoints of the `Server`.
fn metric_server_api() -> ApiDescription<ProducerRegistry> {
    let mut api = ApiDescription::new();
    api.register(collect_endpoint)
        .expect("Failed to register handler for collect_endpoint");
    api
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, Serialize)]
pub struct ProducerIdPathParams {
    pub producer_id: Uuid,
}

// Implementation of the actual collection routine used by the `Server`.
#[endpoint {
    method = GET,
    path = "/collect/{producer_id}",
}]
async fn collect_endpoint(
    request_context: RequestContext<ProducerRegistry>,
    path_params: Path<ProducerIdPathParams>,
) -> Result<HttpResponseOk<ProducerResults>, HttpError> {
    let registry = request_context.context();
    let producer_id = path_params.into_inner().producer_id;
    collect(registry, producer_id).await
}

// TODO this seems misplaced.
/// Register a metric server to be polled for metric data.
///
/// This function is used to provide consumers the flexibility to define their own Dropshot
/// servers, rather than using the `Server` provided by this crate (which starts a _new_ server).
pub async fn register(
    address: SocketAddr,
    log: &slog::Logger,
    server_info: &omicron_common::api::internal::nexus::ProducerEndpoint,
) -> Result<(), Error> {
    let client =
        nexus_client::Client::new(&format!("http://{}", address), log.clone());
    client
        .cpapi_producers_post(&server_info.into())
        .await
        .map(|_| ())
        .map_err(|msg| Error::RegistrationError(msg.to_string()))
}

/// Handle a request to pull available metric data from a [`ProducerRegistry`].
pub async fn collect(
    registry: &ProducerRegistry,
    producer_id: Uuid,
) -> Result<HttpResponseOk<ProducerResults>, HttpError> {
    if producer_id == registry.producer_id() {
        Ok(HttpResponseOk(registry.collect()))
    } else {
        Err(HttpError::for_not_found(
            None,
            format!(
                "Producer ID {} is not valid, expected {}",
                producer_id,
                registry.producer_id()
            ),
        ))
    }
}
