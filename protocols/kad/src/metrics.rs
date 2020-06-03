/*
 * Copyright 2020 Fluence Labs Limited
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use prometheus::{IntCounterVec, Registry, Opts, Counter, Gauge, IntGauge};

pub struct Metrics {
    sent_requests: IntCounterVec,
    received_responses: IntCounterVec,
    received_requests: IntCounterVec,
    sent_responses: IntCounterVec,
    errors: IntCounterVec,
    records_stored: IntGauge,
}

impl Metrics {
    pub fn new(registry: &Registry) -> Self {
        let default = Opts::new("", "").namespace("libp2p").subsystem("kad");

        let opts = |name: &str| -> Opts {
            let mut opts = default.clone();
            opts.name = name.into();
            opts.help = name.into(); // TODO: better help?
            opts
        };

        // Creates and registers counter in registry
        let counter = |name: &str, label_names: &[&str]| -> IntCounterVec {
            let counter = IntCounterVec::new(
                opts(name),
                label_names
            ).expect(format!("create {}", name).as_str());

            registry.register(Box::new(counter.clone())).expect(format!("register {}", name).as_str());
            counter
        };

        let requests = &["find_node", "get_providers", "add_provider", "get_record", "put_record"];
        let responses = &["find_node", "get_providers", "get_record", "put_record"];
        let errors = &["todo"]; // TODO: fill error types

        let sent_requests = counter("sent_requests", requests);
        let received_requests = counter("received_requests", requests);
        let sent_responses = counter("sent_responses", responses);
        let received_responses = counter("received_responses", responses);
        let errors = counter("errors", errors);
        let records_stored = IntGauge::with_opts(opts("records_stored")).expect("create records_stored");
        registry.register(Box::new(records_stored.clone())).expect("register records_stored");

        Self {
            sent_requests,
            received_responses,
            received_requests,
            sent_responses,
            errors,
            records_stored
        }
    }
}