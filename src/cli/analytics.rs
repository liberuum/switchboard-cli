use anyhow::Result;
use clap::Subcommand;
use serde_json::Value;

use crate::cli::helpers;
use crate::output::{OutputFormat, print_json, print_table};

#[derive(Subcommand)]
pub enum AnalyticsCommand {
    /// List available metrics
    Metrics,
    /// List available dimensions and their values
    Dimensions,
    /// List available currencies
    Currencies,
    /// Query analytics time series
    Series {
        /// Start date (e.g. 2026-01-01)
        #[arg(long)]
        start: Option<String>,
        /// End date (e.g. 2026-12-31)
        #[arg(long)]
        end: Option<String>,
        /// Granularity (e.g. HOURLY, DAILY, WEEKLY, MONTHLY, ANNUALLY, TOTAL)
        #[arg(long)]
        granularity: Option<String>,
        /// Metrics to include (comma-separated)
        #[arg(long)]
        metrics: Option<String>,
        /// Currency code
        #[arg(long)]
        currency: Option<String>,
    },
}

pub async fn run(cmd: AnalyticsCommand, format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    match cmd {
        AnalyticsCommand::Metrics => metrics(format, profile_name).await,
        AnalyticsCommand::Dimensions => dimensions(format, profile_name).await,
        AnalyticsCommand::Currencies => currencies(format, profile_name).await,
        AnalyticsCommand::Series {
            start,
            end,
            granularity,
            metrics,
            currency,
        } => series(start, end, granularity, metrics, currency, format, profile_name).await,
    }
}

async fn metrics(format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    let data = client
        .query("{ analytics { metrics } }", None)
        .await?;

    let metrics = data
        .pointer("/analytics/metrics")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&Value::Array(metrics)),
        _ => {
            if metrics.is_empty() {
                println!("No metrics available.");
                return Ok(());
            }
            for m in &metrics {
                println!("  {}", m.as_str().unwrap_or("-"));
            }
        }
    }

    Ok(())
}

async fn dimensions(format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    let data = client
        .query("{ analytics { dimensions { name values { path label } } } }", None)
        .await?;

    let dims = data
        .pointer("/analytics/dimensions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&Value::Array(dims)),
        _ => {
            if dims.is_empty() {
                println!("No dimensions available.");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = dims
                .iter()
                .map(|d| {
                    let name = d["name"].as_str().unwrap_or("-").to_string();
                    let vals = d["values"]
                        .as_array()
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| {
                                    v["label"]
                                        .as_str()
                                        .or_else(|| v["path"].as_str())
                                })
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .unwrap_or_default();
                    vec![name, vals]
                })
                .collect();
            print_table(&["Dimension", "Values"], &rows);
        }
    }

    Ok(())
}

async fn currencies(format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    let data = client
        .query("{ analytics { currencies } }", None)
        .await?;

    let currencies = data
        .pointer("/analytics/currencies")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&Value::Array(currencies)),
        _ => {
            if currencies.is_empty() {
                println!("No currencies available.");
                return Ok(());
            }
            for c in &currencies {
                println!("  {}", c.as_str().unwrap_or("-"));
            }
        }
    }

    Ok(())
}

async fn series(
    start: Option<String>,
    end: Option<String>,
    granularity: Option<String>,
    metrics: Option<String>,
    currency: Option<String>,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    // Build filter arguments
    let mut filter_parts = Vec::new();
    if let Some(ref s) = start {
        filter_parts.push(format!("start: \"{}\"", s.replace('"', r#"\""#)));
    }
    if let Some(ref e) = end {
        filter_parts.push(format!("end: \"{}\"", e.replace('"', r#"\""#)));
    }
    if let Some(ref g) = granularity {
        // Granularity is an enum — send unquoted
        filter_parts.push(format!("granularity: {g}"));
    }
    if let Some(ref m) = metrics {
        let list: String = m
            .split(',')
            .map(|s| format!("\"{}\"", s.trim()))
            .collect::<Vec<_>>()
            .join(", ");
        filter_parts.push(format!("metrics: [{list}]"));
    }
    if let Some(ref c) = currency {
        filter_parts.push(format!("currency: \"{}\"", c.replace('"', r#"\""#)));
    }

    let filter_arg = if filter_parts.is_empty() {
        String::new()
    } else {
        format!("(filter: {{ {} }})", filter_parts.join(", "))
    };

    let query = format!(
        r#"{{ analytics {{ series{filter_arg} {{ period start end rows {{ metric value unit sum }} }} }} }}"#
    );

    let data = client.query(&query, None).await?;

    let series = data
        .pointer("/analytics/series")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&Value::Array(series)),
        _ => {
            if series.is_empty() {
                println!("No analytics data found.");
                return Ok(());
            }
            for period in &series {
                let label = period["period"].as_str().unwrap_or("-");
                println!("Period: {label}");
                if let Some(rows) = period["rows"].as_array() {
                    let table_rows: Vec<Vec<String>> = rows
                        .iter()
                        .map(|r| {
                            let metric = r["metric"].as_str().unwrap_or("-").to_string();
                            let value = match &r["value"] {
                                Value::Number(n) => n.to_string(),
                                Value::String(s) => s.clone(),
                                other => other.to_string(),
                            };
                            let unit = r["unit"].as_str().unwrap_or("").to_string();
                            vec![metric, value, unit]
                        })
                        .collect();
                    print_table(&["Metric", "Value", "Unit"], &table_rows);
                }
                println!();
            }
        }
    }

    Ok(())
}
