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

use ballista::prelude::*;
use ballista_examples::test_util;
use datafusion::{error::DataFusionError, prelude::SessionContext};

#[tokio::main]
async fn main() -> datafusion::error::Result<()> {
    let _ = env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .parse_filters(
            " ballista_scheduler-rs=debug,ballista_executor=debug,datafusion=debug",
        )
        .is_test(true)
        .try_init();

    let testdata = test_util::examples_test_data();

    let config = BallistaConfig::new()
        .map_err(|e| DataFusionError::Configuration(e.to_string()))?;

    let ctx = SessionContext::ballista_standalone(&config).await?;
    ctx.register_parquet(
        "test",
        &format!("{testdata}/alltypes_plain.parquet"),
        Default::default(),
    )
    .await?;

    // let df = ctx.sql("select count(1) from test").await?;
    // df.show().await?;

    ctx.sql("select * from test")
        .await?
        .write_csv("../target/p/", Default::default(), Default::default())
        .await?;

    ctx.sql("select * from test").await?.show().await?;

    Ok(())
}
