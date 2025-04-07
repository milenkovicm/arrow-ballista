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

use std::sync::Arc;

use ballista::extension::SessionContextExt;
use datafusion::arrow::array::{ArrayRef, Float64Array};
use datafusion::arrow::datatypes::DataType;
use datafusion::common::cast::as_float64_array;
use datafusion::error::Result;
use datafusion::logical_expr::{ColumnarValue, Volatility};
use datafusion::prelude::create_udf;
use datafusion::{assert_batches_eq, prelude::SessionContext};

#[tokio::main]
async fn main() -> Result<()> {
    let test_data = ballista_examples::test_util::examples_test_data();
    let ctx: SessionContext = SessionContext::remote("df://localhost:50050").await?;

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

    ctx.register_udf(pow.clone());

    ctx.register_parquet(
        "test",
        &format!("{test_data}/alltypes_plain.parquet"),
        Default::default(),
    )
    .await?;

    let result = ctx
        .sql("select my_pow(10.0, double_col) as udf_call from test where id > 4")
        .await?
        .collect()
        .await?;

    let expected = [
        "+--------------------+",
        "| udf_call           |",
        "+--------------------+",
        "| 12589254117.941662 |",
        "| 1.0                |",
        "| 12589254117.941662 |",
        "+--------------------+",
    ];

    assert_batches_eq!(expected, &result);
    Ok(())
}
