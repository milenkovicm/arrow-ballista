// Licensed to the Apache Software Foundation (ASF) under one
// or more contributor license agreements.  See the NOTICE file
// distributed with this work for additional information
// regarding copyright ownership.  The ASF licenses this file
// to you under the Apache License, Version 2.0 (the
// "License"); you may not use this file except in compliance
// with the License.  You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing,
// software distributed under the License is distributed on an
// "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied.  See the License for the
// specific language governing permissions and limitations
// under the License.

use ballista_core::error::BallistaError;
use ballista_core::registry::BallistaFunctionRegistry;
use ballista_scheduler::cluster::BallistaCluster;
use ballista_scheduler::config::SchedulerConfig;
use ballista_scheduler::scheduler_process::start_server;
use datafusion::arrow::array::{ArrayRef, Float64Array};
use datafusion::arrow::datatypes::DataType;
use datafusion::common::cast::as_float64_array;
use datafusion::execution::runtime_env::RuntimeEnvBuilder;
use datafusion::execution::SessionStateBuilder;
use datafusion::logical_expr::{ColumnarValue, Volatility};
use datafusion::prelude::create_udf;
use std::net::AddrParseError;
use std::sync::Arc;

///
/// # Custom Ballista Scheduler
///
/// This example demonstrates how to crate custom ballista schedulers.
///
#[tokio::main]
async fn main() -> ballista_core::error::Result<()> {
    let _ = env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .is_test(true)
        .try_init();

    let pow = Arc::new(|args: &[ColumnarValue]| {
        assert_eq!(args.len(), 2);
        let args = ColumnarValue::values_to_arrays(args)?;

        let base = as_float64_array(&args[0]).expect("cast failed");
        let exponent = as_float64_array(&args[1]).expect("cast failed");

        assert_eq!(exponent.len(), base.len());
        let array = base
            .iter()
            .zip(exponent.iter())
            .map(|(base, exponent)| match (base, exponent) {
                (Some(base), Some(exponent)) => Some(base.powf(exponent)),
                _ => None,
            })
            .collect::<Float64Array>();
        Ok(ColumnarValue::from(Arc::new(array) as ArrayRef))
    });

    let pow = create_udf(
        "my_pow",
        vec![DataType::Float64, DataType::Float64],
        DataType::Float64,
        Volatility::Immutable,
        pow,
    );
    let mut registry = BallistaFunctionRegistry::default();
    registry
        .scalar_functions
        .insert("my_pow".to_string(), Arc::new(pow));
    let registry = Arc::new(registry);
    
    let config: SchedulerConfig = SchedulerConfig {
        override_session_builder: Some(Arc::new(move |config| {
            let state = SessionStateBuilder::new()
                .with_default_features()
                .with_config(config)
                .with_runtime_env(Arc::new(RuntimeEnvBuilder::new().build()?))
                .with_scalar_functions(
                    registry.scalar_functions.values().cloned().collect(),
                )
                .build();
            //let mut ctx = default_session_builder(c)?;

            Ok(state)
        })),
        ..Default::default()
    };

    let addr = format!("{}:{}", config.bind_host, config.bind_port);
    let addr = addr
        .parse()
        .map_err(|e: AddrParseError| BallistaError::Configuration(e.to_string()))?;

    let cluster = BallistaCluster::new_from_config(&config).await?;
    start_server(cluster, addr, Arc::new(config)).await?;

    Ok(())
}
