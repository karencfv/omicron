// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Background task for garbage collecting metrics producers that have not
//! renewed their lease

use crate::app::background::BackgroundTask;
use chrono::TimeDelta;
use chrono::Utc;
use futures::FutureExt;
use futures::future::BoxFuture;
use nexus_db_queries::context::OpContext;
use nexus_db_queries::db::DataStore;
use serde_json::json;
use slog_error_chain::InlineErrorChain;
use std::sync::Arc;
use std::time::Duration;

/// Background task that prunes metrics producers that have failed to renew
/// their lease.
pub struct MetricProducerGc {
    datastore: Arc<DataStore>,
    lease_duration: Duration,
}

impl MetricProducerGc {
    pub fn new(datastore: Arc<DataStore>, lease_duration: Duration) -> Self {
        Self { datastore, lease_duration }
    }

    async fn activate(&mut self, opctx: &OpContext) -> serde_json::Value {
        let Some(expiration) = TimeDelta::from_std(self.lease_duration)
            .ok()
            .and_then(|delta| Utc::now().checked_sub_signed(delta))
        else {
            error!(
                opctx.log,
                "Metric producer GC: out of bounds lease_duration";
                "lease_duration" => ?self.lease_duration,
            );
            return json!({
                "error": "out of bounds lease duration",
                "lease_duration": self.lease_duration,
            });
        };

        info!(
            opctx.log, "Metric producer GC running";
            "expiration" => %expiration,
        );
        let pruned = match nexus_metrics_producer_gc::prune_expired_producers(
            opctx,
            &self.datastore,
            expiration,
        )
        .await
        {
            Ok(pruned) => pruned,
            Err(err) => {
                warn!(opctx.log, "Metric producer GC failed"; &err);
                return json!({
                    "error": InlineErrorChain::new(&err).to_string(),
                });
            }
        };

        if pruned.failures.is_empty() {
            info!(
                opctx.log, "Metric producer GC complete (no errors)";
                "expiration" => %expiration,
                "pruned" => ?pruned.successes,
            );
            json!({
                "expiration": expiration,
                "pruned": pruned.successes,
            })
        } else {
            warn!(
                opctx.log,
                "Metric producer GC complete ({} errors)",
                pruned.failures.len();
                "expiration" => %expiration,
                "pruned" => ?pruned.successes,
                "failures" => ?pruned.failures,
            );
            json!({
                "expiration": expiration,
                "pruned": pruned.successes,
                "errors": pruned.failures,
            })
        }
    }
}

impl BackgroundTask for MetricProducerGc {
    fn activate<'a>(
        &'a mut self,
        opctx: &'a OpContext,
    ) -> BoxFuture<'a, serde_json::Value> {
        self.activate(opctx).boxed()
    }
}

#[cfg(test)]
mod tests {
    use crate::app::oximeter::PRODUCER_LEASE_DURATION;

    use super::*;
    use async_bb8_diesel::AsyncRunQueryDsl;
    use chrono::DateTime;
    use chrono::Utc;
    use diesel::ExpressionMethods;
    use httptest::Expectation;
    use httptest::matchers::request;
    use httptest::responders::status_code;
    use nexus_db_model::OximeterInfo;
    use nexus_db_queries::context::OpContext;
    use nexus_test_utils_macros::nexus_test;
    use nexus_types::internal_api::params;
    use omicron_common::api::external::DataPageParams;
    use omicron_common::api::internal::nexus;
    use omicron_common::api::internal::nexus::ProducerRegistrationResponse;
    use serde_json::json;
    use uuid::Uuid;

    type ControlPlaneTestContext =
        nexus_test_utils::ControlPlaneTestContext<crate::Server>;

    async fn set_time_modified(
        datastore: &DataStore,
        producer_id: Uuid,
        time_modified: DateTime<Utc>,
    ) {
        use nexus_db_schema::schema::metric_producer::dsl;

        let conn = datastore.pool_connection_for_tests().await.unwrap();
        if let Err(err) = diesel::update(dsl::metric_producer)
            .filter(dsl::id.eq(producer_id))
            .set(dsl::time_modified.eq(time_modified))
            .execute_async(&*conn)
            .await
        {
            panic!(
                "failed to update time_modified for producer {producer_id}: \
                 {err}"
            );
        }
    }

    #[nexus_test(server = crate::Server)]
    async fn test_pruning(cptestctx: &ControlPlaneTestContext) {
        let nexus = &cptestctx.server.server_context().nexus;
        let datastore = nexus.datastore();
        let opctx = OpContext::for_tests(
            cptestctx.logctx.log.clone(),
            datastore.clone(),
        );

        // Producer <-> collector assignment is random. We're going to create a
        // mock collector below then insert a producer, and we want to guarantee
        // the producer is assigned to the mock collector. To do so, we need to
        // expunge the "real" collector set up by `nexus_test`. We'll phrase
        // this as a loop to match the datastore methods and in case nexus_test
        // ever starts multiple collectors.
        for oximeter_info in datastore
            .oximeter_list(&opctx, &DataPageParams::max_page())
            .await
            .expect("listed oximeters")
        {
            datastore
                .oximeter_expunge(&opctx, oximeter_info.id)
                .await
                .expect("expunged oximeter");
        }

        let mut collector = httptest::Server::run();

        // There are several producers which automatically register themselves
        // during tests, from Nexus and the simulated sled-agent for example. We
        // don't particularly care about these registrations, so ignore any such
        // requests to our simulated collector server.
        let body = serde_json::to_string(&ProducerRegistrationResponse {
            lease_duration: PRODUCER_LEASE_DURATION,
        })
        .unwrap();
        collector.expect(
            Expectation::matching(request::method_path("POST", "/producers"))
                .times(0..)
                .respond_with(status_code(201).body(body)),
        );

        // Insert an Oximeter collector
        let collector_info = OximeterInfo::new(&params::OximeterInfo {
            collector_id: Uuid::new_v4(),
            address: collector.addr(),
        });
        datastore
            .oximeter_create(&opctx, &collector_info)
            .await
            .expect("failed to insert collector");

        // Insert a producer.
        let producer = nexus::ProducerEndpoint {
            id: Uuid::new_v4(),
            kind: nexus::ProducerKind::Service,
            address: "[::1]:0".parse().unwrap(), // unused
            interval: Duration::from_secs(0),    // unused
        };
        datastore
            .producer_endpoint_upsert_and_assign(&opctx, &producer)
            .await
            .expect("failed to insert producer");

        // Create the task and activate it. Technically this is racy in that it
        // could prune the producer we just added, but if it's been an hour
        // since then, we have bigger problems. This should _not_ prune the
        // producer, since it's been active within the last hour.
        let mut gc =
            MetricProducerGc::new(datastore.clone(), Duration::from_secs(3600));
        let value = gc.activate(&opctx).await;
        let value = value.as_object().expect("non-object");
        assert!(!value.contains_key("failures"));
        assert!(value.contains_key("expiration"));
        assert_eq!(*value.get("pruned").expect("missing `pruned`"), json!([]));

        // Move our producer backwards in time: pretend it registered two hours
        // ago, which should result in it being pruned.
        set_time_modified(
            &datastore,
            producer.id,
            Utc::now() - chrono::TimeDelta::hours(2),
        )
        .await;

        // Pruning should also notify the collector.
        collector.expect(
            Expectation::matching(request::method_path(
                "DELETE",
                format!("/producers/{}", producer.id),
            ))
            .respond_with(status_code(204)),
        );

        let value = gc.activate(&opctx).await;
        let value = value.as_object().expect("non-object");
        assert!(!value.contains_key("failures"));
        assert!(value.contains_key("expiration"));
        assert_eq!(
            *value.get("pruned").expect("missing `pruned`"),
            json!([producer.id])
        );

        collector.verify_and_clear();
    }
}
