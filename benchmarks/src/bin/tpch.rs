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

//! Benchmark derived from TPC-H. This is not an official TPC-H benchmark.

use ballista::extension::SessionConfigExt;
use ballista::prelude::SessionContextExt;
use datafusion::arrow::array::*;
use datafusion::arrow::datatypes::SchemaBuilder;
use datafusion::arrow::util::display::array_value_to_string;
use datafusion::common::{DEFAULT_CSV_EXTENSION, DEFAULT_PARQUET_EXTENSION};
use datafusion::datasource::listing::ListingTableUrl;
use datafusion::datasource::{MemTable, TableProvider};
use datafusion::error::{DataFusionError, Result};
use datafusion::execution::context::SessionState;
use datafusion::execution::SessionStateBuilder;
use datafusion::logical_expr::LogicalPlan;
use datafusion::logical_expr::{expr::Cast, Expr};
use datafusion::parquet::basic::Compression;
use datafusion::parquet::file::properties::WriterProperties;
use datafusion::physical_plan::display::DisplayableExecutionPlan;
use datafusion::physical_plan::{collect, displayable};
use datafusion::prelude::*;
use datafusion::{
    arrow::datatypes::{DataType, Field, Schema},
    datasource::file_format::{csv::CsvFormat, FileFormat},
    DATAFUSION_VERSION,
};
use datafusion::{
    arrow::record_batch::RecordBatch, datasource::file_format::parquet::ParquetFormat,
};
use datafusion::{
    arrow::util::pretty,
    datasource::listing::{ListingOptions, ListingTable, ListingTableConfig},
};
use futures::future::join_all;
use rand::prelude::*;
use serde::Serialize;
use std::ops::Div;
use std::{
    fs::{self, File},
    io::Write,
    iter::Iterator,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Instant, SystemTime},
};
use structopt::StructOpt;
use tokio::task::JoinHandle;

#[cfg(feature = "snmalloc")]
#[global_allocator]
static ALLOC: snmalloc_rs::SnMalloc = snmalloc_rs::SnMalloc;

#[cfg(feature = "mimalloc")]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Debug, StructOpt, Clone)]
struct BallistaBenchmarkOpt {
    /// Query number
    #[structopt(short, long)]
    query: usize,

    /// Activate debug mode to see query results
    #[structopt(short, long)]
    debug: bool,

    /// Path to expected results
    #[structopt(short = "e", long = "expected")]
    expected_results: Option<String>,

    /// Number of iterations of each test run
    #[structopt(short = "i", long = "iterations", default_value = "3")]
    iterations: usize,

    /// Batch size when reading CSV or Parquet files
    #[structopt(short = "s", long = "batch-size", default_value = "8192")]
    batch_size: usize,

    /// Path to data files
    #[structopt(parse(from_os_str), required = true, short = "p", long = "path")]
    path: PathBuf,

    /// File format: `csv` or `parquet`
    #[structopt(short = "f", long = "format", default_value = "csv")]
    file_format: String,

    // /// Load the data into a MemTable before executing the query
    // #[structopt(short = "m", long = "mem-table")]
    // mem_table: bool,
    /// Number of partitions to process in parallel
    #[structopt(short = "n", long = "partitions", default_value = "2")]
    partitions: usize,

    /// Ballista executor host
    #[structopt(long = "host")]
    host: Option<String>,

    /// Ballista executor port
    #[structopt(long = "port")]
    port: Option<u16>,

    /// Path to output directory where JSON summary file should be written to
    #[structopt(parse(from_os_str), short = "o", long = "output")]
    output_path: Option<PathBuf>,
}

#[derive(Debug, StructOpt, Clone)]
struct DataFusionBenchmarkOpt {
    /// Query number
    #[structopt(short, long)]
    query: usize,

    /// Activate debug mode to see query results
    #[structopt(short, long)]
    debug: bool,

    /// Number of iterations of each test run
    #[structopt(short = "i", long = "iterations", default_value = "3")]
    iterations: usize,

    /// Number of partitions to process in parallel
    #[structopt(short = "n", long = "partitions", default_value = "2")]
    partitions: usize,

    /// Batch size when reading CSV or Parquet files
    #[structopt(short = "s", long = "batch-size", default_value = "8192")]
    batch_size: usize,

    /// Path to data files
    #[structopt(parse(from_os_str), required = true, short = "p", long = "path")]
    path: PathBuf,

    /// File format: `csv` or `parquet`
    #[structopt(short = "f", long = "format", default_value = "csv")]
    file_format: String,

    /// Load the data into a MemTable before executing the query
    #[structopt(short = "m", long = "mem-table")]
    mem_table: bool,

    /// Path to output directory where JSON summary file should be written to
    #[structopt(parse(from_os_str), short = "o", long = "output")]
    output_path: Option<PathBuf>,
}

#[derive(Debug, StructOpt, Clone)]
struct BallistaLoadtestOpt {
    #[structopt(short = "q", long)]
    query_list: String,

    /// Activate debug mode to see query results
    #[structopt(short, long)]
    debug: bool,

    /// Number of requests
    #[structopt(short = "r", long = "requests", default_value = "100")]
    requests: usize,

    /// Number of connections
    #[structopt(short = "c", long = "concurrency", default_value = "5")]
    concurrency: usize,

    /// Number of partitions to process in parallel
    #[structopt(short = "n", long = "partitions", default_value = "2")]
    partitions: usize,

    /// Batch size when reading CSV or Parquet files
    #[structopt(short = "s", long = "batch-size", default_value = "8192")]
    batch_size: usize,

    /// Path to data files
    #[structopt(parse(from_os_str), required = true, short = "p", long = "data-path")]
    path: PathBuf,

    /// Path to sql files
    #[structopt(parse(from_os_str), required = true, long = "sql-path")]
    sql_path: PathBuf,

    /// File format: `csv` or `parquet`
    #[structopt(short = "f", long = "format", default_value = "parquet")]
    file_format: String,

    /// Ballista executor host
    #[structopt(long = "host")]
    host: Option<String>,

    /// Ballista executor port
    #[structopt(long = "port")]
    port: Option<u16>,
}

#[derive(Debug, StructOpt)]
struct ConvertOpt {
    /// Path to csv files
    #[structopt(parse(from_os_str), required = true, short = "i", long = "input")]
    input_path: PathBuf,

    /// Output path
    #[structopt(parse(from_os_str), required = true, short = "o", long = "output")]
    output_path: PathBuf,

    /// Output file format: `csv` or `parquet`
    #[structopt(short = "f", long = "format")]
    file_format: String,

    /// Compression to use when writing Parquet files
    #[structopt(short = "c", long = "compression", default_value = "zstd")]
    compression: String,

    /// Number of partitions to produce
    #[structopt(short = "n", long = "partitions", default_value = "1")]
    partitions: usize,

    /// Batch size when reading CSV or Parquet files
    #[structopt(short = "s", long = "batch-size", default_value = "8192")]
    batch_size: usize,
}

#[derive(Debug, StructOpt)]
#[structopt(about = "benchmark command")]
enum BenchmarkSubCommandOpt {
    #[structopt(name = "ballista")]
    BallistaBenchmark(BallistaBenchmarkOpt),
    #[structopt(name = "datafusion")]
    DataFusionBenchmark(DataFusionBenchmarkOpt),
}

#[derive(Debug, StructOpt)]
#[structopt(about = "loadtest command")]
enum LoadtestOpt {
    #[structopt(name = "ballista-load")]
    BallistaLoadtest(BallistaLoadtestOpt),
}

#[derive(Debug, StructOpt)]
#[structopt(name = "TPC-H", about = "TPC-H Benchmarks.")]
enum TpchOpt {
    Benchmark(BenchmarkSubCommandOpt),
    Convert(ConvertOpt),
    Loadtest(LoadtestOpt),
}

const TABLES: &[&str] = &[
    "part", "supplier", "partsupp", "customer", "orders", "lineitem", "nation", "region",
];

#[tokio::main]
async fn main() -> Result<()> {
    use BenchmarkSubCommandOpt::*;
    use LoadtestOpt::*;

    env_logger::init();
    match TpchOpt::from_args() {
        TpchOpt::Benchmark(BallistaBenchmark(opt)) => {
            benchmark_ballista(opt).await.map(|_| ())
        }
        TpchOpt::Benchmark(DataFusionBenchmark(opt)) => {
            benchmark_datafusion(opt).await.map(|_| ())
        }
        TpchOpt::Convert(opt) => convert_tbl(opt).await,
        TpchOpt::Loadtest(BallistaLoadtest(opt)) => {
            loadtest_ballista(opt).await.map(|_| ())
        }
    }
}

#[allow(clippy::await_holding_lock)]
async fn benchmark_datafusion(opt: DataFusionBenchmarkOpt) -> Result<Vec<RecordBatch>> {
    println!("Running benchmarks with the following options: {opt:?}");
    let mut benchmark_run = BenchmarkRun::new(opt.query);
    let config = SessionConfig::new()
        .with_target_partitions(opt.partitions)
        .with_batch_size(opt.batch_size);
    let ctx = SessionContext::new_with_config(config);

    // register tables
    for table in TABLES {
        let table_provider = {
            let mut session_state = ctx.state();
            get_table(
                &mut session_state,
                opt.path.to_str().unwrap(),
                table,
                opt.file_format.as_str(),
                opt.partitions,
            )
            .await?
        };
        if opt.mem_table {
            println!("Loading table '{table}' into memory");
            let start = Instant::now();
            let memtable =
                MemTable::load(table_provider, Some(opt.partitions), &ctx.state())
                    .await?;
            println!(
                "Loaded table '{}' into memory in {} ms",
                table,
                start.elapsed().as_millis()
            );
            ctx.register_table(*table, Arc::new(memtable))?;
        } else {
            ctx.register_table(*table, table_provider)?;
        }
    }

    let mut millis = vec![];
    // run benchmark
    let mut result: Vec<RecordBatch> = Vec::with_capacity(1);
    for i in 0..opt.iterations {
        let start = Instant::now();
        let plans = create_logical_plans(&ctx, opt.query).await?;
        for plan in plans {
            result = execute_query(&ctx, &plan, opt.debug).await?;
        }
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        millis.push(elapsed);
        let row_count = result.iter().map(|b| b.num_rows()).sum();
        println!(
            "Query {} iteration {} took {:.1} ms and returned {} rows",
            opt.query, i, elapsed, row_count
        );
        benchmark_run.add_result(elapsed, row_count);
    }

    let avg = millis.iter().sum::<f64>() / millis.len() as f64;
    println!("Query {} avg time: {:.2} ms", opt.query, avg);

    if let Some(path) = &opt.output_path {
        write_summary_json(&mut benchmark_run, path)?;
    }

    Ok(result)
}

async fn benchmark_ballista(opt: BallistaBenchmarkOpt) -> Result<()> {
    println!("Running benchmarks with the following options: {opt:?}");
    let mut benchmark_run = BenchmarkRun::new(opt.query);

    let config = SessionConfig::new_with_ballista()
        .with_target_partitions(opt.partitions)
        .with_ballista_job_name(&format!("Query derived from TPC-H q{}", opt.query))
        .with_batch_size(opt.batch_size)
        .with_collect_statistics(true);

    let state = SessionStateBuilder::new()
        .with_default_features()
        .with_config(config)
        .build();
    let address = format!(
        "df://{}:{}",
        opt.host.clone().unwrap().as_str(),
        opt.port.unwrap()
    );
    let ctx = SessionContext::remote_with_state(&address, state).await?;

    // register tables with Ballista context
    let path = opt.path.to_str().unwrap();
    let file_format = opt.file_format.as_str();

    register_tables(path, file_format, &ctx, opt.debug).await?;

    let mut millis = vec![];

    // run benchmark
    let queries = get_query_sql(opt.query)?;
    println!(
        "Running benchmark with queries {}:\n {:?}",
        opt.query, queries
    );
    let mut batches = vec![];
    for i in 0..opt.iterations {
        let start = Instant::now();
        for sql in &queries {
            let df = ctx
                .sql(sql)
                .await
                .map_err(|e| DataFusionError::Plan(format!("{e:?}")))
                .unwrap();
            let plan = df.clone().into_optimized_plan()?;
            if opt.debug {
                println!("=== Optimized logical plan ===\n{plan:?}\n");
            }
            batches = df
                .collect()
                .await
                .map_err(|e| DataFusionError::Plan(format!("{e:?}")))
                .unwrap();
        }
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        millis.push(elapsed);
        let row_count = batches.iter().map(|b| b.num_rows()).sum();
        println!(
            "Query {} iteration {} took {:.1} ms and returned {} rows",
            opt.query, i, elapsed, row_count
        );
        benchmark_run.add_result(elapsed, row_count);
        if opt.debug {
            pretty::print_batches(&batches)?;
        }

        if let Some(expected_results_path) = opt.expected_results.as_ref() {
            let expected = get_expected_results(opt.query, expected_results_path).await?;
            assert_expected_results(&expected, &batches)
        }
    }

    let avg = millis.iter().sum::<f64>() / millis.len() as f64;
    println!("Query {} avg time: {:.2} ms", opt.query, avg);

    if let Some(path) = &opt.output_path {
        write_summary_json(&mut benchmark_run, path)?;
    }

    Ok(())
}

fn write_summary_json(benchmark_run: &mut BenchmarkRun, path: &Path) -> Result<()> {
    let json =
        serde_json::to_string_pretty(&benchmark_run).expect("summary is serializable");
    let filename = format!(
        "tpch-q{}-{}.json",
        benchmark_run.query, benchmark_run.start_time
    );
    let path = path.join(filename);
    println!(
        "Writing summary file to {}",
        path.as_os_str().to_str().unwrap()
    );
    let mut file = File::create(path)?;
    file.write_all(json.as_bytes())?;
    Ok(())
}

async fn loadtest_ballista(opt: BallistaLoadtestOpt) -> Result<()> {
    println!("Running loadtest_ballista with the following options: {opt:?}");

    let config = SessionConfig::new_with_ballista()
        .with_target_partitions(opt.partitions)
        .with_batch_size(opt.batch_size);

    let state = SessionStateBuilder::new()
        .with_default_features()
        .with_config(config)
        .build();

    let concurrency = opt.concurrency;
    let request_amount = opt.requests;
    let mut clients = vec![];

    for _num in 0..concurrency {
        let address = format!(
            "df://{}:{}",
            opt.host.clone().unwrap().as_str(),
            opt.port.unwrap()
        );
        clients.push(SessionContext::remote_with_state(&address, state.clone()).await?);
    }

    // register tables with Ballista context
    let path = opt.path.to_str().unwrap();
    let file_format = opt.file_format.as_str();
    let sql_path = opt.sql_path.to_str().unwrap().to_string();

    for ctx in &clients {
        register_tables(path, file_format, ctx, opt.debug).await?;
    }

    let request_per_thread = request_amount.div(concurrency);
    // run benchmark
    let query_list: Vec<usize> = opt
        .query_list
        .split(',')
        .map(|s| s.parse().unwrap())
        .collect();
    println!("query list: {:?} ", &query_list);

    let total = Instant::now();
    let mut futures = vec![];

    for (client_id, client) in clients.into_iter().enumerate() {
        let query_list_clone = query_list.clone();
        let sql_path_clone = sql_path.clone();
        let handle = tokio::spawn(async move {
            for i in 0..request_per_thread {
                let query_id = query_list_clone
                    .get(
                        (0..query_list_clone.len())
                            .choose(&mut rand::rng())
                            .unwrap(),
                    )
                    .unwrap();
                let sql =
                    get_query_sql_by_path(query_id.to_owned(), sql_path_clone.clone())
                        .unwrap();
                println!(
                    "Client {} Round {} Query {} started",
                    &client_id, &i, query_id
                );
                let start = Instant::now();
                let df = client
                    .sql(&sql)
                    .await
                    .map_err(|e| DataFusionError::Plan(format!("{e:?}")))
                    .unwrap();
                let batches = df
                    .collect()
                    .await
                    .map_err(|e| DataFusionError::Plan(format!("{e:?}")))
                    .unwrap();
                let elapsed = start.elapsed().as_secs_f64() * 1000.0;
                println!(
                    "Client {} Round {} Query {} took {:.1} ms ",
                    &client_id, &i, query_id, elapsed
                );
                if opt.debug && !batches.is_empty() {
                    pretty::print_batches(&batches).unwrap();
                }
            }
        });
        futures.push(handle);
    }
    join_all(futures).await;
    let elapsed = total.elapsed().as_secs_f64() * 1000.0;
    println!("###############################");
    println!("load test  took {elapsed:.1} ms");
    Ok(())
}

fn get_query_sql_by_path(query: usize, mut sql_path: String) -> Result<String> {
    if sql_path.ends_with('/') {
        sql_path.pop();
    }
    if query > 0 && query < 23 {
        let filename = format!("{sql_path}/q{query}.sql");
        Ok(fs::read_to_string(filename).expect("failed to read query"))
    } else {
        Err(DataFusionError::Plan(
            "invalid query. Expected value between 1 and 22".to_owned(),
        ))
    }
}

async fn register_tables(
    path: &str,
    file_format: &str,
    ctx: &SessionContext,
    debug: bool,
) -> Result<()> {
    for &table in TABLES {
        match file_format {
            // dbgen creates .tbl ('|' delimited) files without header
            "tbl" => {
                let path = find_path(path, table, "tbl")?;
                let schema = get_tbl_tpch_table_schema(table);
                let options = CsvReadOptions::new()
                    .schema(&schema)
                    .delimiter(b'|')
                    .has_header(false)
                    .file_extension(".tbl");
                if debug {
                    println!(
                        "Registering table '{table}' using TBL files at path {path}"
                    );
                }
                ctx.register_csv(table, &path, options)
                    .await
                    .map_err(|e| DataFusionError::Plan(format!("{e:?}")))?;
            }
            "csv" => {
                let path = find_path(path, table, "csv")?;
                let schema = get_schema(table);
                let options = CsvReadOptions::new().schema(&schema).has_header(true);
                if debug {
                    println!(
                        "Registering table '{table}' using CSV files at path {path}"
                    );
                }
                ctx.register_csv(table, &path, options)
                    .await
                    .map_err(|e| DataFusionError::Plan(format!("{e:?}")))?;
            }
            "parquet" => {
                let path = find_path(path, table, "parquet")?;
                if debug {
                    println!(
                        "Registering table '{table}' using Parquet files at path {path}"
                    );
                }
                ctx.register_parquet(table, &path, ParquetReadOptions::default())
                    .await
                    .map_err(|e| DataFusionError::Plan(format!("{e:?}")))?;
            }
            other => {
                return Err(DataFusionError::Plan(format!(
                    "Invalid file format '{other}'"
                )))
            }
        }
    }
    Ok(())
}

fn find_path(path: &str, table: &str, ext: &str) -> Result<String> {
    let path1 = format!("{path}/{table}.{ext}");
    let path2 = format!("{path}/{table}");
    if Path::new(&path1).exists() {
        Ok(path1)
    } else if Path::new(&path2).exists() {
        Ok(path2)
    } else {
        Err(DataFusionError::Plan(format!(
            "Could not find {ext} files at {path1} or {path2}"
        )))
    }
}

/// Get the SQL statements from the specified query file
fn get_query_sql(query: usize) -> Result<Vec<String>> {
    if query > 0 && query < 23 {
        let possibilities = vec![
            format!("queries/q{query}.sql"),
            format!("benchmarks/queries/q{query}.sql"),
        ];
        let mut errors = vec![];
        for filename in possibilities {
            match fs::read_to_string(&filename) {
                Ok(contents) => {
                    return Ok(contents
                        .split(';')
                        .map(|s| s.trim())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                        .collect())
                }
                Err(e) => errors.push(format!("{filename}: {e}")),
            };
        }
        Err(DataFusionError::Plan(format!(
            "invalid query. Could not find query: {errors:?}"
        )))
    } else {
        Err(DataFusionError::Plan(
            "invalid query. Expected value between 1 and 22".to_owned(),
        ))
    }
}

/// Create a logical plan for each query in the specified query file
async fn create_logical_plans(
    ctx: &SessionContext,
    query: usize,
) -> Result<Vec<LogicalPlan>> {
    let session_state = ctx.state();
    let sqls = get_query_sql(query)?;
    let join_handles = sqls
        .into_iter()
        .map(|sql| {
            let session_state = session_state.clone();
            tokio::spawn(
                async move { session_state.create_logical_plan(sql.as_str()).await },
            )
        })
        .collect::<Vec<JoinHandle<Result<LogicalPlan>>>>();
    futures::future::join_all(join_handles)
        .await
        .into_iter()
        .collect::<std::result::Result<Vec<Result<LogicalPlan>>, tokio::task::JoinError>>(
        )
        .map_err(|e| DataFusionError::Internal(format!("{e:?}")))?
        .into_iter()
        .collect()
}

async fn execute_query(
    ctx: &SessionContext,
    plan: &LogicalPlan,
    debug: bool,
) -> Result<Vec<RecordBatch>> {
    if debug {
        println!("=== Logical plan ===\n{plan:?}\n");
    }
    let session_state = ctx.state();
    let plan = session_state.optimize(plan)?;
    if debug {
        println!("=== Optimized logical plan ===\n{plan:?}\n");
    }
    let physical_plan = session_state.create_physical_plan(&plan).await?;
    if debug {
        println!(
            "=== Physical plan ===\n{}\n",
            displayable(physical_plan.as_ref()).indent(false)
        );
    }
    let task_ctx = ctx.task_ctx();
    let result = collect(physical_plan.clone(), task_ctx).await?;
    if debug {
        println!(
            "=== Physical plan with metrics ===\n{}\n",
            DisplayableExecutionPlan::with_metrics(physical_plan.as_ref()).indent(false)
        );
        if !result.is_empty() {
            pretty::print_batches(&result)?;
        }
    }
    Ok(result)
}

async fn convert_tbl(opt: ConvertOpt) -> Result<()> {
    let output_root_path = Path::new(&opt.output_path);
    for table in TABLES {
        let start = Instant::now();
        let schema = get_schema(table);

        let input_path = format!("{}/{}.tbl", opt.input_path.to_str().unwrap(), table);
        let options = CsvReadOptions::new()
            .schema(&schema)
            .delimiter(b'|')
            .file_extension(".tbl");

        let config = SessionConfig::new().with_batch_size(opt.batch_size);
        let ctx = SessionContext::new_with_config(config);
        let session_state = ctx.state();

        // build plan to read the TBL file
        let mut csv = ctx.read_csv(&input_path, options).await?;

        // optionally, repartition the file
        if opt.partitions > 1 {
            csv = csv.repartition(Partitioning::RoundRobinBatch(opt.partitions))?
        }

        // create the physical plan
        let csv = csv.into_optimized_plan()?;
        let csv = session_state.optimize(&csv)?;
        let csv = session_state.create_physical_plan(&csv).await?;

        let output_path = output_root_path.join(table);
        let output_path = output_path.to_str().unwrap().to_owned();

        println!(
            "Converting '{}' to {} files in directory '{}'",
            &input_path, &opt.file_format, &output_path
        );
        match opt.file_format.as_str() {
            "csv" => ctx.write_csv(csv, output_path).await?,
            "parquet" => {
                let compression = match opt.compression.as_str() {
                    "none" => Compression::UNCOMPRESSED,
                    "snappy" => Compression::SNAPPY,
                    "brotli" => Compression::BROTLI(Default::default()),
                    "gzip" => Compression::GZIP(Default::default()),
                    "lz4" => Compression::LZ4,
                    "lz0" => Compression::LZO,
                    "zstd" => Compression::ZSTD(Default::default()),
                    other => {
                        return Err(DataFusionError::NotImplemented(format!(
                            "Invalid compression format: {other}"
                        )))
                    }
                };
                let props = WriterProperties::builder()
                    .set_compression(compression)
                    .build();
                ctx.write_parquet(csv, output_path, Some(props)).await?
            }
            other => {
                return Err(DataFusionError::NotImplemented(format!(
                    "Invalid output format: {other}"
                )))
            }
        }
        println!("Conversion completed in {} ms", start.elapsed().as_millis());
    }

    Ok(())
}

async fn get_table(
    ctx: &mut SessionState,
    path: &str,
    table: &str,
    table_format: &str,
    target_partitions: usize,
) -> Result<Arc<dyn TableProvider>> {
    let (format, path, extension, schema): (
        Arc<dyn FileFormat>,
        String,
        &'static str,
        Schema,
    ) = match table_format {
        // dbgen creates .tbl ('|' delimited) files without header
        "tbl" => {
            let path = format!("{path}/{table}.tbl");

            let format = CsvFormat::default()
                .with_delimiter(b'|')
                .with_has_header(false);

            (
                Arc::new(format),
                path,
                ".tbl",
                get_tbl_tpch_table_schema(table),
            )
        }
        "csv" => {
            let path = format!("{path}/{table}");
            let format = CsvFormat::default()
                .with_delimiter(b',')
                .with_has_header(true);

            (
                Arc::new(format),
                path,
                DEFAULT_CSV_EXTENSION,
                get_schema(table),
            )
        }
        "parquet" => {
            let path = format!("{path}/{table}");
            let format = ParquetFormat::default().with_enable_pruning(true);

            (
                Arc::new(format),
                path,
                DEFAULT_PARQUET_EXTENSION,
                get_schema(table),
            )
        }
        other => {
            unimplemented!("Invalid file format '{}'", other);
        }
    };

    let options = ListingOptions {
        format,
        file_extension: extension.to_owned(),
        target_partitions,
        collect_stat: true,
        table_partition_cols: vec![],
        file_sort_order: vec![],
    };

    let url = ListingTableUrl::parse(path)?;
    let config = ListingTableConfig::new(url).with_listing_options(options);

    let config = if table_format == "parquet" {
        config.infer_schema(ctx).await?
    } else {
        config.with_schema(Arc::new(schema))
    };

    Ok(Arc::new(ListingTable::try_new(config)?))
}

fn get_schema(table: &str) -> Schema {
    // note that the schema intentionally uses signed integers so that any generated Parquet
    // files can also be used to benchmark tools that only support signed integers, such as
    // Apache Spark

    match table {
        "part" => Schema::new(vec![
            Field::new("p_partkey", DataType::Int64, false),
            Field::new("p_name", DataType::Utf8, false),
            Field::new("p_mfgr", DataType::Utf8, false),
            Field::new("p_brand", DataType::Utf8, false),
            Field::new("p_type", DataType::Utf8, false),
            Field::new("p_size", DataType::Int32, false),
            Field::new("p_container", DataType::Utf8, false),
            Field::new("p_retailprice", DataType::Decimal128(15, 2), false),
            Field::new("p_comment", DataType::Utf8, false),
        ]),

        "supplier" => Schema::new(vec![
            Field::new("s_suppkey", DataType::Int64, false),
            Field::new("s_name", DataType::Utf8, false),
            Field::new("s_address", DataType::Utf8, false),
            Field::new("s_nationkey", DataType::Int64, false),
            Field::new("s_phone", DataType::Utf8, false),
            Field::new("s_acctbal", DataType::Decimal128(15, 2), false),
            Field::new("s_comment", DataType::Utf8, false),
        ]),

        "partsupp" => Schema::new(vec![
            Field::new("ps_partkey", DataType::Int64, false),
            Field::new("ps_suppkey", DataType::Int64, false),
            Field::new("ps_availqty", DataType::Int32, false),
            Field::new("ps_supplycost", DataType::Decimal128(15, 2), false),
            Field::new("ps_comment", DataType::Utf8, false),
        ]),

        "customer" => Schema::new(vec![
            Field::new("c_custkey", DataType::Int64, false),
            Field::new("c_name", DataType::Utf8, false),
            Field::new("c_address", DataType::Utf8, false),
            Field::new("c_nationkey", DataType::Int64, false),
            Field::new("c_phone", DataType::Utf8, false),
            Field::new("c_acctbal", DataType::Decimal128(15, 2), false),
            Field::new("c_mktsegment", DataType::Utf8, false),
            Field::new("c_comment", DataType::Utf8, false),
        ]),

        "orders" => Schema::new(vec![
            Field::new("o_orderkey", DataType::Int64, false),
            Field::new("o_custkey", DataType::Int64, false),
            Field::new("o_orderstatus", DataType::Utf8, false),
            Field::new("o_totalprice", DataType::Decimal128(15, 2), false),
            Field::new("o_orderdate", DataType::Date32, false),
            Field::new("o_orderpriority", DataType::Utf8, false),
            Field::new("o_clerk", DataType::Utf8, false),
            Field::new("o_shippriority", DataType::Int32, false),
            Field::new("o_comment", DataType::Utf8, false),
        ]),

        "lineitem" => Schema::new(vec![
            Field::new("l_orderkey", DataType::Int64, false),
            Field::new("l_partkey", DataType::Int64, false),
            Field::new("l_suppkey", DataType::Int64, false),
            Field::new("l_linenumber", DataType::Int32, false),
            Field::new("l_quantity", DataType::Decimal128(15, 2), false),
            Field::new("l_extendedprice", DataType::Decimal128(15, 2), false),
            Field::new("l_discount", DataType::Decimal128(15, 2), false),
            Field::new("l_tax", DataType::Decimal128(15, 2), false),
            Field::new("l_returnflag", DataType::Utf8, false),
            Field::new("l_linestatus", DataType::Utf8, false),
            Field::new("l_shipdate", DataType::Date32, false),
            Field::new("l_commitdate", DataType::Date32, false),
            Field::new("l_receiptdate", DataType::Date32, false),
            Field::new("l_shipinstruct", DataType::Utf8, false),
            Field::new("l_shipmode", DataType::Utf8, false),
            Field::new("l_comment", DataType::Utf8, false),
        ]),

        "nation" => Schema::new(vec![
            Field::new("n_nationkey", DataType::Int64, false),
            Field::new("n_name", DataType::Utf8, false),
            Field::new("n_regionkey", DataType::Int64, false),
            Field::new("n_comment", DataType::Utf8, false),
        ]),

        "region" => Schema::new(vec![
            Field::new("r_regionkey", DataType::Int64, false),
            Field::new("r_name", DataType::Utf8, false),
            Field::new("r_comment", DataType::Utf8, false),
        ]),

        _ => unimplemented!(),
    }
}

/// The `.tbl` file contains a trailing column
pub fn get_tbl_tpch_table_schema(table: &str) -> Schema {
    let mut schema = SchemaBuilder::from(get_schema(table).fields);
    schema.push(Field::new("__placeholder", DataType::Utf8, false));
    schema.finish()
}

#[derive(Debug, Serialize)]
struct BenchmarkRun {
    /// Benchmark crate version
    benchmark_version: String,
    /// DataFusion crate version
    datafusion_version: String,
    /// Number of CPU cores
    num_cpus: usize,
    /// Start time
    start_time: u64,
    /// CLI arguments
    arguments: Vec<String>,
    /// query number
    query: usize,
    /// list of individual run times and row counts
    iterations: Vec<QueryResult>,
}

impl BenchmarkRun {
    fn new(query: usize) -> Self {
        Self {
            benchmark_version: env!("CARGO_PKG_VERSION").to_owned(),
            datafusion_version: DATAFUSION_VERSION.to_owned(),
            num_cpus: std::thread::available_parallelism().unwrap().get(),
            start_time: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("current time is later than UNIX_EPOCH")
                .as_secs(),
            arguments: std::env::args().skip(1).collect::<Vec<String>>(),
            query,
            iterations: vec![],
        }
    }

    fn add_result(&mut self, elapsed: f64, row_count: usize) {
        self.iterations.push(QueryResult { elapsed, row_count })
    }
}

/// Compare actual results against expected results at scale factor 1
fn assert_expected_results(expected: &[RecordBatch], actual: &[RecordBatch]) {
    // assert schema equality without comparing nullable values
    assert_eq!(
        nullable_schema(expected[0].schema()),
        nullable_schema(actual[0].schema())
    );

    // convert both datasets to Vec<Vec<String>> for simple comparison
    let expected_vec = result_vec(expected);
    let actual_vec = result_vec(actual);

    // basic result comparison
    assert_eq!(expected_vec.len(), actual_vec.len());

    // compare each row. this works as all TPC-H queries have deterministically ordered results
    for i in 0..actual_vec.len() {
        assert_eq!(expected_vec[i], actual_vec[i]);
    }
}

/// Get the expected answer for a specific query at scale factor 1
async fn get_expected_results(n: usize, path: &str) -> Result<Vec<RecordBatch>> {
    let ctx = SessionContext::new();
    let schema = string_schema(get_answer_schema(n));
    let options = CsvReadOptions::new()
        .schema(&schema)
        .delimiter(b'|')
        .file_extension(".out");
    let answer_path = format!("{path}/answers/q{n}.out");
    println!("Looking for expected results at {answer_path}");
    let df = ctx.read_csv(&answer_path, options).await?;
    let df = df.select(
        get_answer_schema(n)
            .fields()
            .iter()
            .map(|field| {
                match Field::data_type(field) {
                    DataType::Decimal128(_, _) => {
                        // there's no support for casting from Utf8 to Decimal, so
                        // we'll cast from Utf8 to Float64 to Decimal for Decimal types
                        let inner_cast = Box::new(Expr::Cast(Cast::new(
                            Box::new(trim(vec![col(Field::name(field))])),
                            DataType::Float64,
                        )));
                        Expr::Cast(Cast::new(
                            inner_cast,
                            Field::data_type(field).to_owned(),
                        ))
                        .alias(Field::name(field))
                    }
                    _ => Expr::Cast(Cast::new(
                        Box::new(trim(vec![col(Field::name(field))])),
                        Field::data_type(field).to_owned(),
                    ))
                    .alias(Field::name(field)),
                }
            })
            .collect::<Vec<Expr>>(),
    )?;
    df.collect().await
}

// convert the schema to the same but with all columns set to nullable=true.
// this allows direct schema comparison ignoring nullable.
fn nullable_schema(schema: Arc<Schema>) -> Schema {
    Schema::new(
        schema
            .fields()
            .iter()
            .map(|field| {
                Field::new(Field::name(field), Field::data_type(field).to_owned(), true)
            })
            .collect::<Vec<Field>>(),
    )
}

/// Converts the results into a 2d array of strings, `result[row][column]`
/// Special cases nulls to NULL for testing
fn result_vec(results: &[RecordBatch]) -> Vec<Vec<String>> {
    let mut result = vec![];
    for batch in results {
        for row_index in 0..batch.num_rows() {
            let row_vec = batch
                .columns()
                .iter()
                .map(|column| col_str(column, row_index))
                .collect();
            result.push(row_vec);
        }
    }
    result
}

fn get_answer_schema(n: usize) -> Schema {
    match n {
        1 => Schema::new(vec![
            Field::new("l_returnflag", DataType::Utf8, true),
            Field::new("l_linestatus", DataType::Utf8, true),
            Field::new("sum_qty", DataType::Decimal128(15, 2), true),
            Field::new("sum_base_price", DataType::Decimal128(15, 2), true),
            Field::new("sum_disc_price", DataType::Decimal128(15, 2), true),
            Field::new("sum_charge", DataType::Decimal128(15, 2), true),
            Field::new("avg_qty", DataType::Decimal128(15, 2), true),
            Field::new("avg_price", DataType::Decimal128(15, 2), true),
            Field::new("avg_disc", DataType::Decimal128(15, 2), true),
            Field::new("count_order", DataType::Int64, true),
        ]),

        2 => Schema::new(vec![
            Field::new("s_acctbal", DataType::Decimal128(15, 2), true),
            Field::new("s_name", DataType::Utf8, true),
            Field::new("n_name", DataType::Utf8, true),
            Field::new("p_partkey", DataType::Int64, true),
            Field::new("p_mfgr", DataType::Utf8, true),
            Field::new("s_address", DataType::Utf8, true),
            Field::new("s_phone", DataType::Utf8, true),
            Field::new("s_comment", DataType::Utf8, true),
        ]),

        3 => Schema::new(vec![
            Field::new("l_orderkey", DataType::Int64, true),
            Field::new("revenue", DataType::Decimal128(15, 2), true),
            Field::new("o_orderdate", DataType::Date32, true),
            Field::new("o_shippriority", DataType::Int32, true),
        ]),

        4 => Schema::new(vec![
            Field::new("o_orderpriority", DataType::Utf8, true),
            Field::new("order_count", DataType::Int64, true),
        ]),

        5 => Schema::new(vec![
            Field::new("n_name", DataType::Utf8, true),
            Field::new("revenue", DataType::Decimal128(15, 2), true),
        ]),

        6 => Schema::new(vec![Field::new(
            "revenue",
            DataType::Decimal128(15, 2),
            true,
        )]),

        7 => Schema::new(vec![
            Field::new("supp_nation", DataType::Utf8, true),
            Field::new("cust_nation", DataType::Utf8, true),
            Field::new("l_year", DataType::Int32, true),
            Field::new("revenue", DataType::Decimal128(15, 2), true),
        ]),

        8 => Schema::new(vec![
            Field::new("o_year", DataType::Int32, true),
            Field::new("mkt_share", DataType::Decimal128(15, 2), true),
        ]),

        9 => Schema::new(vec![
            Field::new("nation", DataType::Utf8, true),
            Field::new("o_year", DataType::Int32, true),
            Field::new("sum_profit", DataType::Decimal128(15, 2), true),
        ]),

        10 => Schema::new(vec![
            Field::new("c_custkey", DataType::Int64, true),
            Field::new("c_name", DataType::Utf8, true),
            Field::new("revenue", DataType::Decimal128(15, 2), true),
            Field::new("c_acctbal", DataType::Decimal128(15, 2), true),
            Field::new("n_name", DataType::Utf8, true),
            Field::new("c_address", DataType::Utf8, true),
            Field::new("c_phone", DataType::Utf8, true),
            Field::new("c_comment", DataType::Utf8, true),
        ]),

        11 => Schema::new(vec![
            Field::new("ps_partkey", DataType::Int64, true),
            Field::new("value", DataType::Decimal128(15, 2), true),
        ]),

        12 => Schema::new(vec![
            Field::new("l_shipmode", DataType::Utf8, true),
            Field::new("high_line_count", DataType::Int64, true),
            Field::new("low_line_count", DataType::Int64, true),
        ]),

        13 => Schema::new(vec![
            Field::new("c_count", DataType::Int64, true),
            Field::new("custdist", DataType::Int64, true),
        ]),

        14 => Schema::new(vec![Field::new("promo_revenue", DataType::Float64, true)]),

        15 => Schema::new(vec![
            Field::new("s_suppkey", DataType::Int64, true),
            Field::new("s_name", DataType::Utf8, true),
            Field::new("s_address", DataType::Utf8, true),
            Field::new("s_phone", DataType::Utf8, true),
            Field::new("total_revenue", DataType::Decimal128(15, 2), true),
        ]),

        16 => Schema::new(vec![
            Field::new("p_brand", DataType::Utf8, true),
            Field::new("p_type", DataType::Utf8, true),
            Field::new("p_size", DataType::Int32, true),
            Field::new("supplier_cnt", DataType::Int64, true),
        ]),

        17 => Schema::new(vec![Field::new("avg_yearly", DataType::Float64, true)]),

        18 => Schema::new(vec![
            Field::new("c_name", DataType::Utf8, true),
            Field::new("c_custkey", DataType::Int64, true),
            Field::new("o_orderkey", DataType::Int64, true),
            Field::new("o_orderdate", DataType::Date32, true),
            Field::new("o_totalprice", DataType::Decimal128(15, 2), true),
            Field::new("sum_l_quantity", DataType::Decimal128(15, 2), true),
        ]),

        19 => Schema::new(vec![Field::new(
            "revenue",
            DataType::Decimal128(15, 2),
            true,
        )]),

        20 => Schema::new(vec![
            Field::new("s_name", DataType::Utf8, true),
            Field::new("s_address", DataType::Utf8, true),
        ]),

        21 => Schema::new(vec![
            Field::new("s_name", DataType::Utf8, true),
            Field::new("numwait", DataType::Int64, true),
        ]),

        22 => Schema::new(vec![
            Field::new("cntrycode", DataType::Utf8, true),
            Field::new("numcust", DataType::Int64, true),
            Field::new("totacctbal", DataType::Decimal128(15, 2), true),
        ]),

        _ => unimplemented!(),
    }
}

/// convert expected schema to all utf8 so columns can be read as strings to be parsed separately
/// this is due to the fact that the csv parser cannot handle leading/trailing spaces
fn string_schema(schema: Schema) -> Schema {
    Schema::new(
        schema
            .fields()
            .iter()
            .map(|field| {
                Field::new(
                    Field::name(field),
                    DataType::Utf8,
                    Field::is_nullable(field),
                )
            })
            .collect::<Vec<Field>>(),
    )
}

/// Specialised String representation
fn col_str(column: &ArrayRef, row_index: usize) -> String {
    if column.is_null(row_index) {
        return "NULL".to_string();
    }

    // Special case ListArray as there is no pretty print support for it yet
    if let DataType::FixedSizeList(_, n) = column.data_type() {
        let array = column
            .as_any()
            .downcast_ref::<FixedSizeListArray>()
            .unwrap()
            .value(row_index);

        let mut r = Vec::with_capacity(*n as usize);
        for i in 0..*n {
            r.push(col_str(&array, i as usize));
        }
        return format!("[{}]", r.join(","));
    }

    array_value_to_string(column, row_index).unwrap()
}

#[derive(Debug, Serialize)]
struct QueryResult {
    elapsed: f64,
    row_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::Arc;

    #[tokio::test]
    async fn q1() -> Result<()> {
        verify_query(1).await
    }

    #[tokio::test]
    async fn q2() -> Result<()> {
        verify_query(2).await
    }

    #[tokio::test]
    async fn q3() -> Result<()> {
        verify_query(3).await
    }

    #[tokio::test]
    async fn q4() -> Result<()> {
        verify_query(4).await
    }

    #[tokio::test]
    async fn q5() -> Result<()> {
        verify_query(5).await
    }

    #[tokio::test]
    async fn q6() -> Result<()> {
        verify_query(6).await
    }

    #[tokio::test]
    async fn q7() -> Result<()> {
        verify_query(7).await
    }

    #[tokio::test]
    async fn q8() -> Result<()> {
        verify_query(8).await
    }

    #[tokio::test]
    async fn q9() -> Result<()> {
        verify_query(9).await
    }

    #[tokio::test]
    async fn q10() -> Result<()> {
        verify_query(10).await
    }

    #[tokio::test]
    async fn q11() -> Result<()> {
        verify_query(11).await
    }

    #[tokio::test]
    async fn q12() -> Result<()> {
        verify_query(12).await
    }

    #[tokio::test]
    async fn q13() -> Result<()> {
        verify_query(13).await
    }

    #[tokio::test]
    async fn q14() -> Result<()> {
        verify_query(14).await
    }

    #[ignore] // TODO: support multiline queries
    #[tokio::test]
    async fn q15() -> Result<()> {
        verify_query(15).await
    }

    #[tokio::test]
    async fn q16() -> Result<()> {
        verify_query(16).await
    }

    // Python code to reproduce the "348406.05" result in DuckDB:
    // ```python
    // import duckdb
    // lineitem = duckdb.read_csv("data/lineitem.tbl", columns={'l_orderkey':'int64', 'l_partkey':'int64', 'l_suppkey':'int64', 'l_linenumber':'int64', 'l_quantity':'int64', 'l_extendedprice':'decimal(15,2)', 'l_discount':'decimal(15,2)', 'l_tax':'decimal(15,2)', 'l_returnflag':'varchar','l_linestatus':'varchar', 'l_shipdate':'date', 'l_commitdate':'date', 'l_receiptdate':'date', 'l_shipinstruct':'varchar', 'l_shipmode':'varchar', 'l_comment':'varchar'})
    // part = duckdb.read_csv("data/part.tbl", columns={'p_partkey':'int64', 'p_name':'varchar', 'p_mfgr':'varchar', 'p_brand':'varchar', 'p_type':'varchar', 'p_size':'int64', 'p_container':'varchar', 'p_retailprice':'double', 'p_comment':'varchar'})
    // duckdb.sql("select sum(l_extendedprice) / 7.0 as avg_yearly from lineitem, part where p_partkey = l_partkey and p_brand = 'Brand#23' and p_container = 'MED BOX' and l_quantity < (select 0.2 * avg(l_quantity) from lineitem where l_partkey = p_partkey )")
    // ```
    // That is the same as DataFusion's output.
    #[ignore = "the expected result is 348406.02 whereas both DataFusion and DuckDB return 348406.05"]
    #[tokio::test]
    async fn q17() -> Result<()> {
        verify_query(17).await
    }

    #[tokio::test]
    async fn q18() -> Result<()> {
        verify_query(18).await
    }

    #[tokio::test]
    async fn q19() -> Result<()> {
        verify_query(19).await
    }

    #[tokio::test]
    async fn q20() -> Result<()> {
        verify_query(20).await
    }

    #[tokio::test]
    async fn q21() -> Result<()> {
        verify_query(21).await
    }

    #[tokio::test]
    async fn q22() -> Result<()> {
        verify_query(22).await
    }

    #[tokio::test]
    async fn run_q1() -> Result<()> {
        run_query(1).await
    }

    #[tokio::test]
    async fn run_2() -> Result<()> {
        run_query(2).await
    }

    #[tokio::test]
    async fn run_q3() -> Result<()> {
        run_query(3).await
    }

    #[tokio::test]
    async fn run_q4() -> Result<()> {
        run_query(4).await
    }

    #[tokio::test]
    async fn run_q5() -> Result<()> {
        run_query(5).await
    }

    #[tokio::test]
    async fn run_q6() -> Result<()> {
        run_query(6).await
    }

    #[tokio::test]
    async fn run_q7() -> Result<()> {
        run_query(7).await
    }

    #[tokio::test]
    async fn run_q8() -> Result<()> {
        run_query(8).await
    }

    #[tokio::test]
    async fn run_q9() -> Result<()> {
        run_query(9).await
    }

    #[tokio::test]
    async fn run_q10() -> Result<()> {
        run_query(10).await
    }

    #[tokio::test]
    async fn run_q11() -> Result<()> {
        run_query(11).await
    }

    #[tokio::test]
    async fn run_q12() -> Result<()> {
        run_query(12).await
    }

    #[tokio::test]
    async fn run_q13() -> Result<()> {
        run_query(13).await
    }

    #[tokio::test]
    async fn run_q14() -> Result<()> {
        run_query(14).await
    }

    #[ignore] // TODO: support multiline queries
    #[tokio::test]
    async fn run_q15() -> Result<()> {
        run_query(15).await
    }

    #[tokio::test]
    async fn run_q16() -> Result<()> {
        run_query(16).await
    }

    #[tokio::test]
    async fn run_q17() -> Result<()> {
        run_query(17).await
    }

    #[tokio::test]
    async fn run_q18() -> Result<()> {
        run_query(18).await
    }

    #[tokio::test]
    async fn run_q19() -> Result<()> {
        run_query(19).await
    }

    #[tokio::test]
    async fn run_q20() -> Result<()> {
        run_query(20).await
    }

    #[tokio::test]
    async fn run_q21() -> Result<()> {
        run_query(21).await
    }

    #[tokio::test]
    async fn run_q22() -> Result<()> {
        run_query(22).await
    }

    async fn run_query(n: usize) -> Result<()> {
        // Tests running query with empty tables, to see whether they run successfully.

        let config = SessionConfig::new()
            .with_target_partitions(1)
            .with_batch_size(10);
        let ctx = SessionContext::new_with_config(config);

        for &table in TABLES {
            let schema = get_schema(table);
            let batch = RecordBatch::new_empty(Arc::new(schema.to_owned()));

            let provider = MemTable::try_new(Arc::new(schema), vec![vec![batch]])?;

            ctx.register_table(table, Arc::new(provider))?;
        }

        let plans = create_logical_plans(&ctx, n).await?;
        for plan in plans {
            execute_query(&ctx, &plan, false).await?;
        }

        Ok(())
    }

    // We read the expected results from CSV files so we need to normalize the
    // query results before we compare them with the expected results for the
    // following reasons:
    //
    // 1. Float numbers have only two digits after the decimal point in CSV so
    // we need to convert results to Decimal(15, 2) and then back to floats.
    //
    // 2. Decimal numbers are fixed as Decimal(15, 2) in CSV.
    //
    // 3. Strings may have trailing spaces and need to be trimmed.
    //
    // 4. Rename columns using the expected schema to make schema matching
    // because, for q18, we have aggregate field `sum(l_quantity)` that is
    // called `sum_l_quantity` in the expected results.
    async fn normalize_for_verification(
        batches: Vec<RecordBatch>,
        expected_schema: Schema,
    ) -> Result<Vec<RecordBatch>> {
        if batches.is_empty() {
            return Ok(vec![]);
        }
        let ctx = SessionContext::new();
        let schema = batches[0].schema();
        let df = ctx.read_batches(batches)?;
        let df = df.select(
            schema
                .fields()
                .iter()
                .zip(expected_schema.fields())
                .map(|(field, expected_field)| {
                    match Field::data_type(field) {
                        // Normalize decimals to Decimal(15, 2)
                        DataType::Decimal128(_, _) => {
                            // We convert to float64 and then to decimal(15, 2).
                            // Directly converting between Decimals caused test
                            // failures.
                            let inner_cast = Box::new(Expr::Cast(Cast::new(
                                Box::new(col(Field::name(field))),
                                DataType::Float64,
                            )));
                            Expr::Cast(Cast::new(inner_cast, DataType::Decimal128(15, 2)))
                                .alias(Field::name(expected_field))
                        }
                        // Normalize floats to have 2 digits after the decimal point
                        DataType::Float64 => {
                            let inner_cast = Box::new(Expr::Cast(Cast::new(
                                Box::new(col(Field::name(field))),
                                DataType::Decimal128(15, 2),
                            )));
                            Expr::Cast(Cast::new(inner_cast, DataType::Float64))
                                .alias(Field::name(expected_field))
                        }
                        // Normalize strings by trimming trailing spaces.
                        DataType::Utf8 => Expr::Cast(Cast::new(
                            Box::new(trim(vec![col(Field::name(field))])),
                            Field::data_type(field).to_owned(),
                        ))
                        .alias(Field::name(field)),
                        _ => col(Field::name(expected_field)),
                    }
                })
                .collect::<Vec<Expr>>(),
        )?;
        df.collect().await
    }

    async fn verify_query(n: usize) -> Result<()> {
        if let Ok(path) = env::var("TPCH_DATA") {
            // load expected answers from tpch-dbgen
            // read csv as all strings, trim and cast to expected type as the csv string
            // to value parser does not handle data with leading/trailing spaces
            let expected = get_expected_results(n, &path).await?;

            // run the query to compute actual results of the query
            let opt = DataFusionBenchmarkOpt {
                query: n,
                debug: false,
                iterations: 1,
                partitions: 2,
                batch_size: 8192,
                path: PathBuf::from(path.to_string()),
                file_format: "tbl".to_string(),
                mem_table: false,
                output_path: None,
            };
            let actual = benchmark_datafusion(opt).await?;
            let expected_schema = get_answer_schema(n);
            let normalized = normalize_for_verification(actual, expected_schema).await?;

            assert_expected_results(&expected, &normalized)
        } else {
            println!("TPCH_DATA environment variable not set, skipping test");
        }

        Ok(())
    }
}

#[cfg(test)]
#[cfg(feature = "ci")]
mod ballista_round_trip {
    use super::*;
    use ballista_core::serde::BallistaCodec;
    use datafusion::config::TableOptions;
    use datafusion::datasource::listing::ListingTableUrl;
    use datafusion::execution::options::ReadOptions;
    use datafusion::physical_plan::ExecutionPlan;
    use datafusion_proto::logical_plan::AsLogicalPlan;
    use datafusion_proto::physical_plan::AsExecutionPlan;
    use std::env;
    use std::ops::Deref;

    async fn round_trip_logical_plan(n: usize) -> Result<()> {
        let config = SessionConfig::new()
            .with_target_partitions(1)
            .with_batch_size(10);
        let ctx = SessionContext::new_with_config(config);
        let session_state = ctx.state();
        let codec: BallistaCodec<
            datafusion_proto::protobuf::LogicalPlanNode,
            datafusion_proto::protobuf::PhysicalPlanNode,
        > = BallistaCodec::default();

        // set tpch_data_path to dummy value and skip physical plan serde test when TPCH_DATA
        // is not set.
        let tpch_data_path = env::var("TPCH_DATA").unwrap_or_else(|_| "./".to_string());
        let path = ListingTableUrl::parse(tpch_data_path)?;

        for &table in TABLES {
            let schema = get_schema(table);
            let options = CsvReadOptions::new()
                .schema(&schema)
                .delimiter(b'|')
                .has_header(false)
                .file_extension(".tbl");
            let cfg = SessionConfig::new();
            let listing_options =
                options.to_listing_options(&cfg, TableOptions::default());
            let config = ListingTableConfig::new(path.clone())
                .with_listing_options(listing_options)
                .with_schema(Arc::new(schema));
            let provider = ListingTable::try_new(config)?;
            ctx.register_table(table, Arc::new(provider))?;
        }

        // test logical plan round trip
        let plans = create_logical_plans(&ctx, n).await?;
        for plan in plans {
            // test optimized logical plan round trip
            let plan = session_state.optimize(&plan)?;
            let proto: datafusion_proto::protobuf::LogicalPlanNode =
                datafusion_proto::protobuf::LogicalPlanNode::try_from_logical_plan(
                    &plan,
                    codec.logical_extension_codec(),
                )
                .unwrap();
            let round_trip: LogicalPlan = proto
                .try_into_logical_plan(&ctx, codec.logical_extension_codec())
                .unwrap();
            assert_eq!(
                format!("{plan:?}"),
                format!("{round_trip:?}"),
                "optimized logical plan round trip failed"
            );
        }

        Ok(())
    }

    async fn round_trip_physical_plan(n: usize) -> Result<()> {
        let config = SessionConfig::new()
            .with_target_partitions(1)
            .with_batch_size(10);
        let ctx = SessionContext::new_with_config(config);
        let session_state = ctx.state();
        let codec: BallistaCodec<
            datafusion_proto::protobuf::LogicalPlanNode,
            datafusion_proto::protobuf::PhysicalPlanNode,
        > = BallistaCodec::default();

        // set tpch_data_path to dummy value and skip physical plan serde test when TPCH_DATA
        // is not set.
        let tpch_data_path = env::var("TPCH_DATA").unwrap_or_else(|_| "./".to_string());
        let path = ListingTableUrl::parse(tpch_data_path)?;

        for &table in TABLES {
            let schema = get_schema(table);
            let options = CsvReadOptions::new()
                .schema(&schema)
                .delimiter(b'|')
                .has_header(false)
                .file_extension(".tbl");
            let cfg = SessionConfig::new();
            let listing_options =
                options.to_listing_options(&cfg, TableOptions::default());
            let config = ListingTableConfig::new(path.clone())
                .with_listing_options(listing_options)
                .with_schema(Arc::new(schema));
            let provider = ListingTable::try_new(config)?;
            ctx.register_table(table, Arc::new(provider))?;
        }

        // test logical plan round trip
        let plans = create_logical_plans(&ctx, n).await?;
        for plan in plans {
            let plan = session_state.optimize(&plan)?;

            // test physical plan roundtrip
            let physical_plan = session_state.create_physical_plan(&plan).await?;
            let proto: datafusion_proto::protobuf::PhysicalPlanNode =
                datafusion_proto::protobuf::PhysicalPlanNode::try_from_physical_plan(
                    physical_plan.clone(),
                    codec.physical_extension_codec(),
                )
                .unwrap();
            let runtime = ctx.runtime_env();
            let round_trip: Arc<dyn ExecutionPlan> = proto
                .try_into_physical_plan(
                    &ctx,
                    runtime.deref(),
                    codec.physical_extension_codec(),
                )
                .unwrap();
            assert_eq!(
                format!("{}", displayable(physical_plan.as_ref()).indent(false)),
                format!("{}", displayable(round_trip.as_ref()).indent(false)),
                "physical plan round trip failed"
            );
        }

        Ok(())
    }

    macro_rules! test_round_trip_logical {
        ($tn:ident, $query:expr) => {
            #[tokio::test]
            async fn $tn() -> Result<()> {
                round_trip_logical_plan($query).await
            }
        };
    }

    test_round_trip_logical!(q1, 1);
    test_round_trip_logical!(q2, 2);
    test_round_trip_logical!(q3, 3);
    test_round_trip_logical!(q4, 4);
    test_round_trip_logical!(q5, 5);
    test_round_trip_logical!(q6, 6);
    test_round_trip_logical!(q7, 7);
    test_round_trip_logical!(q8, 8);
    test_round_trip_logical!(q9, 9);
    test_round_trip_logical!(q10, 10);
    test_round_trip_logical!(q11, 11);
    test_round_trip_logical!(q12, 12);
    test_round_trip_logical!(q13, 13);
    test_round_trip_logical!(q14, 14);
    //test_round_trip_logical!(q15, 15); // https://github.com/apache/datafusion-ballista/issues/330
    test_round_trip_logical!(q16, 16);
    test_round_trip_logical!(q17, 17);
    test_round_trip_logical!(q18, 18);
    test_round_trip_logical!(q19, 19);
    test_round_trip_logical!(q20, 20);
    test_round_trip_logical!(q21, 21);
    test_round_trip_logical!(q22, 22);

    macro_rules! test_round_trip_physical {
        ($tn:ident, $query:expr) => {
            #[tokio::test]
            async fn $tn() -> Result<()> {
                round_trip_physical_plan($query).await
            }
        };
    }

    test_round_trip_physical!(physical_round_trip_q1, 1);
    test_round_trip_physical!(physical_round_trip_q2, 2);
    test_round_trip_physical!(physical_round_trip_q3, 3);
    test_round_trip_physical!(physical_round_trip_q4, 4);
    test_round_trip_physical!(physical_round_trip_q5, 5);
    test_round_trip_physical!(physical_round_trip_q6, 6);
    test_round_trip_physical!(physical_round_trip_q7, 7);
    test_round_trip_physical!(physical_round_trip_q8, 8);
    test_round_trip_physical!(physical_round_trip_q9, 9);
    test_round_trip_physical!(physical_round_trip_q10, 10);
    test_round_trip_physical!(physical_round_trip_q11, 11);
    test_round_trip_physical!(physical_round_trip_q12, 12);
    test_round_trip_physical!(physical_round_trip_q13, 13);
    test_round_trip_physical!(physical_round_trip_q14, 14);
    // test_round_trip_physical!(physical_round_trip_q15, 15); // https://github.com/apache/datafusion-ballista/issues/330
    test_round_trip_physical!(physical_round_trip_q16, 16);
    test_round_trip_physical!(physical_round_trip_q17, 17);
    test_round_trip_physical!(physical_round_trip_q18, 18);
    test_round_trip_physical!(physical_round_trip_q19, 19);
    test_round_trip_physical!(physical_round_trip_q20, 20);
    test_round_trip_physical!(physical_round_trip_q21, 21);
    test_round_trip_physical!(physical_round_trip_q22, 22);
}
