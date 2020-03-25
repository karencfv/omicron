/*!
 * Generic server-wide state and facilities
 */

use super::error::HttpError;
use super::handler::RequestContext;
use super::http_util::HEADER_REQUEST_ID;
use super::router::HttpRouter;

use futures::lock::Mutex;
use futures::FutureExt;
use hyper::server::conn::AddrStream;
use hyper::service::Service;
use hyper::Body;
use hyper::Request;
use hyper::Response;
use std::any::Any;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;
use uuid::Uuid;

use slog::Logger;

/* TODO Replace this with something else? */
type GenericError = Box<dyn std::error::Error + Send + Sync>;

/**
 * Stores shared state used by the generic HTTP server submodule.
 */
pub struct ServerState {
    /** caller-specific state */
    pub private: Box<dyn Any + Send + Sync + 'static>,
    /** static server configuration parameters */
    pub config: ServerConfig,
    /** request router */
    pub router: HttpRouter,
    /** server-wide log handle */
    pub log: Logger,
}

/**
 * Stores static configuration associated with the server
 */
pub struct ServerConfig {
    /** maximum allowed size of a request body */
    pub request_body_max_bytes: usize,
}

/**
 * A thin wrapper around a Hyper Server object that exposes some interfaces that
 * we find useful (e.g., close()).
 * TODO-cleanup: this mechanism should probably do better with types.  In
 * particular, once you call run(), you shouldn't be able to call it again
 * (i.e., it should consume self).  But you should be able to close() it.  Once
 * you've called close(), you shouldn't be able to call it again.
 */
pub struct HttpServer {
    server_future: Option<
        Pin<Box<dyn Future<Output = Result<(), hyper::error::Error>> + Send>>,
    >,
    local_addr: SocketAddr,
    close_channel: Option<tokio::sync::oneshot::Sender<()>>,
}

impl HttpServer {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr.clone()
    }

    pub fn close(mut self) {
        /*
         * It should be impossible to close a channel that's already been closed
         * because close() consumes self.  It should also be impossible to fail
         * to send the close signal because nothing else can cause the server to
         * exit.
         */
        let channel =
            self.close_channel.take().expect("already closed somehow");
        channel.send(()).expect("failed to send close signal");
    }

    pub fn run(
        &mut self,
    ) -> tokio::task::JoinHandle<Result<(), hyper::error::Error>> {
        let future =
            self.server_future.take().expect("cannot run() more than once");
        tokio::spawn(async { future.await })
    }

    /**
     * Set up an HTTP server bound on the specified address that runs registered
     * handlers.  You must invoke `run()` on the returned instance of
     * `HttpServer` (and await the result) to actually start the server.  You
     * can call `close()` to begin a graceful shutdown of the server, which will
     * be complete when the `run()` Future is resolved.
     */
    pub fn new(
        bind_address: &SocketAddr,
        mut router: HttpRouter,
        private: Box<dyn Any + Send + Sync + 'static>,
        log: Logger,
    ) -> Result<HttpServer, hyper::error::Error> {
        /* TODO-hardening: this should not be built in except under test. */
        test_endpoints::register_test_endpoints(&mut router);

        /* TODO-cleanup too many Arcs? */
        let log_close = log.new(o!());
        let app_state = Arc::new(ServerState {
            private: private,
            config: ServerConfig {
                /* We start aggressively to ensure test coverage. */
                request_body_max_bytes: 1024,
            },
            router: router,
            log: log.new(o!()),
        });

        let make_service = ServerConnectionHandler::new(Arc::clone(&app_state));
        let builder = hyper::Server::try_bind(bind_address)?;
        let server = builder.serve(make_service);
        let local_addr = server.local_addr();
        info!(log, "listening"; "local_addr" => %local_addr);

        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let graceful = server.with_graceful_shutdown(async move {
            rx.await.ok();
            info!(log_close, "received request to begin graceful shutdown");
        });

        Ok(HttpServer {
            server_future: Some(graceful.boxed()),
            local_addr: local_addr,
            close_channel: Some(tx),
        })
    }
}

/**
 * Initial entry point for handling a new connection to the HTTP server.
 * This is invoked by Hyper when a new connection is accepted.  This function
 * must return a Hyper Service object that will handle requests for this
 * connection.
 */
async fn http_connection_handle(
    server: Arc<ServerState>,
    remote_addr: SocketAddr,
) -> Result<ServerRequestHandler, GenericError> {
    info!(server.log, "accepted connection"; "remote_addr" => %remote_addr);
    Ok(ServerRequestHandler::new(server))
}

/**
 * Initial entry point for handling a new request to the HTTP server.  This is
 * invoked by Hyper when a new request is received.  This function returns a
 * Result that either represents a valid HTTP response or an error (which will
 * also get turned into an HTTP response).
 */
async fn http_request_handle_wrap(
    server: Arc<ServerState>,
    request: Request<Body>,
) -> Result<Response<Body>, GenericError> {
    /*
     * This extra level of indirection makes error handling much more
     * straightforward, since the request handling code can simply return early
     * with an error and we'll treat it like an error from any of the endpoints
     * themselves.
     */
    let request_id = generate_request_id();
    let request_log = server.log.new(o!(
        "req_id" => request_id.clone(),
        "method" => request.method().as_str().to_string(),
        "uri" => format!("{}", request.uri()),
    ));
    trace!(request_log, "incoming request");
    let maybe_response = http_request_handle(
        Arc::clone(&server),
        request,
        &request_id,
        request_log.new(o!()),
    )
    .await;

    let response = match maybe_response {
        Err(error) => {
            let message_external = error.external_message.clone();
            let message_internal = error.internal_message.clone();
            let r = error.into_response(&request_id);

            /* TODO-debug: add request and response headers here */
            info!(request_log, "request completed";
                "response_code" => r.status().as_str().to_string(),
                "error_message_internal" => message_internal,
                "error_message_external" => message_external,
            );

            r
        }

        Ok(response) => {
            /* TODO-debug: add request and response headers here */
            info!(request_log, "request completed";
                "response_code" => response.status().as_str().to_string()
            );

            response
        }
    };

    Ok(response)
}

async fn http_request_handle(
    server: Arc<ServerState>,
    request: Request<Body>,
    request_id: &str,
    request_log: Logger,
) -> Result<Response<Body>, HttpError> {
    /*
     * TODO-hardening: is it correct to (and do we correctly) read the entire
     * request body even if we decide it's too large and are going to send a 400
     * response?
     * TODO-hardening: add a request read timeout as well so that we don't allow
     * this to take forever.
     * TODO-correctness: check that URL processing (particularly with slashes as
     * the only separator) is correct.  (Do we need to URL-escape or un-escape
     * here?  Redirect container URls that don't end it "/"?)
     * TODO-correctness: Do we need to dump the body on errors?
     */
    let method = request.method();
    let uri = request.uri();
    let lookup_result = server.router.lookup_route(&method, uri.path())?;
    let rqctx = RequestContext {
        server: Arc::clone(&server),
        request: Arc::new(Mutex::new(request)),
        path_variables: lookup_result.variables,
        request_id: request_id.to_string(),
        log: request_log,
    };
    let mut response = lookup_result.handler.handle_request(rqctx).await?;
    response.headers_mut().insert(
        HEADER_REQUEST_ID,
        http::header::HeaderValue::from_str(&request_id).unwrap(),
    );
    Ok(response)
}

/*
 * This function should probably be parametrized by some name of the service
 * that is expected to be unique within an organization.  That way, it would be
 * possible to determine from a given request id which service it was from.
 * TODO should we encode more information here?  Service?  Instance?  Time up to
 * the hour?
 */
fn generate_request_id() -> String {
    format!("{}", Uuid::new_v4())
}

/**
 * ServerConnectionHandler is a Hyper Service implementation that forwards
 * incoming connections to `http_connection_handle()`, providing the server
 * state object as an additional argument.  We could use `make_service_fn` here
 * using a closure to capture the state object, but the resulting code is a bit
 * simpler without it.
 */
pub struct ServerConnectionHandler {
    /** backend state that will be made available to the connection handler */
    server: Arc<ServerState>,
}

impl ServerConnectionHandler {
    /**
     * Create an ServerConnectionHandler with the given state object that
     * will be made available to the handler.
     */
    fn new(server: Arc<ServerState>) -> Self {
        ServerConnectionHandler {
            server: Arc::clone(&server),
        }
    }
}

impl Service<&AddrStream> for ServerConnectionHandler {
    /*
     * Recall that a Service in this context is just something that takes a
     * request (which could be anything) and produces a response (which could be
     * anything).  This being a connection handler, the request type is an
     * AddrStream (which wraps a TCP connection) and the response type is
     * another Service: one that accepts HTTP requests and produces HTTP
     * responses.
     */
    type Response = ServerRequestHandler;
    type Error = GenericError;
    type Future = Pin<
        Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        // TODO is this right?
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, conn: &AddrStream) -> Self::Future {
        /*
         * We're given a borrowed reference to the AddrStream, but our interface
         * is async (which is good, so that we can support time-consuming
         * operations as part of receiving requests).  To avoid having to ensure
         * that conn's lifetime exceeds that of this async operation, we simply
         * copy the only useful information out of the conn: the SocketAddr.  We
         * may want to create our own connection type to encapsulate the socket
         * address and any other per-connection state that we want to keep.
         */
        let server = Arc::clone(&self.server);
        let remote_addr = conn.remote_addr();
        Box::pin(http_connection_handle(server, remote_addr))
    }
}

/**
 * ServerRequestHandler is a Hyper Service implementation that forwards
 * incoming requests to `http_request_handle_wrap()`, including as an argument
 * the backend server state object.  We could use `service_fn` here using a
 * closure to capture the server state object, but the resulting code is a bit
 * simpler without all that.
 */
pub struct ServerRequestHandler {
    /** backend state that will be made available to the request handler */
    server: Arc<ServerState>,
}

impl ServerRequestHandler {
    /**
     * Create a ServerRequestHandler object with the given state object that
     * will be provided to the handler function.
     */
    fn new(server: Arc<ServerState>) -> Self {
        ServerRequestHandler {
            server: Arc::clone(&server),
        }
    }
}

impl Service<Request<Body>> for ServerRequestHandler {
    type Response = Response<Body>;
    type Error = GenericError;
    type Future = Pin<
        Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        // TODO is this right?
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        Box::pin(http_request_handle_wrap(Arc::clone(&self.server), req))
    }
}

/*
 * Demo handler functions
 * TODO-cleanup these should not be registered except in test mode.  They kind
 * of belong under #[cfg(test)], except that we want to be able to use these
 * from integration tests.
 * Maybe instead we should have the unit / integration test instantiate its own
 * server with handlers like these.
 */
pub mod test_endpoints {
    use super::super::error::HttpError;
    use super::super::handler::HttpRouteHandler;
    use super::super::handler::Json;
    use super::super::handler::Query;
    use super::super::handler::RequestContext;
    use super::super::http_util::CONTENT_TYPE_JSON;
    use super::super::router::HttpRouter;
    use http::StatusCode;
    use hyper::Body;
    use hyper::Method;
    use hyper::Response;
    use serde::Deserialize;
    use serde::Serialize;
    use std::sync::Arc;

    pub fn register_test_endpoints(router: &mut HttpRouter) {
        router.insert(
            Method::GET,
            "/testing/demo1",
            HttpRouteHandler::new(demo_handler_args_1),
        );
        router.insert(
            Method::GET,
            "/testing/demo2query",
            HttpRouteHandler::new(demo_handler_args_2query),
        );
        router.insert(
            Method::GET,
            "/testing/demo2json",
            HttpRouteHandler::new(demo_handler_args_2json),
        );
        router.insert(
            Method::GET,
            "/testing/demo3",
            HttpRouteHandler::new(demo_handler_args_3),
        );
    }

    async fn demo_handler_args_1(
        _rqctx: Arc<RequestContext>,
    ) -> Result<Response<Body>, HttpError> {
        Ok(Response::builder()
            .header(http::header::CONTENT_TYPE, CONTENT_TYPE_JSON)
            .status(StatusCode::OK)
            .body("demo_handler_args_1\n".into())?)
    }

    #[derive(Serialize, Deserialize)]
    pub struct DemoQueryArgs {
        pub test1: String,
        pub test2: Option<u32>,
    }

    async fn demo_handler_args_2query(
        _rqctx: Arc<RequestContext>,
        query: Query<DemoQueryArgs>,
    ) -> Result<Response<Body>, HttpError> {
        Ok(Response::builder()
            .header(http::header::CONTENT_TYPE, CONTENT_TYPE_JSON)
            .status(StatusCode::OK)
            .body(serde_json::to_string(&query.into_inner()).unwrap().into())?)
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub struct DemoJsonBody {
        pub test1: String,
        pub test2: Option<u32>,
    }

    async fn demo_handler_args_2json(
        _rqctx: Arc<RequestContext>,
        json: Json<DemoJsonBody>,
    ) -> Result<Response<Body>, HttpError> {
        Ok(Response::builder()
            .header(http::header::CONTENT_TYPE, CONTENT_TYPE_JSON)
            .status(StatusCode::OK)
            .body(serde_json::to_string(&json.into_inner()).unwrap().into())?)
    }

    #[derive(Deserialize, Serialize)]
    pub struct DemoJsonAndQuery {
        pub query: DemoQueryArgs,
        pub json: DemoJsonBody,
    }
    async fn demo_handler_args_3(
        _rqctx: Arc<RequestContext>,
        query: Query<DemoQueryArgs>,
        json: Json<DemoJsonBody>,
    ) -> Result<Response<Body>, HttpError> {
        let combined = DemoJsonAndQuery {
            query: query.into_inner(),
            json: json.into_inner(),
        };
        Ok(Response::builder()
            .header(http::header::CONTENT_TYPE, CONTENT_TYPE_JSON)
            .status(StatusCode::OK)
            .body(serde_json::to_string(&combined).unwrap().into())?)
    }
}
