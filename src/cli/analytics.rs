use anyhow::Result;
use clap::{Args, Subcommand};
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
    Series(SeriesArgs),
}

#[derive(Args)]
pub struct SeriesArgs {
    /// Start date (e.g. 2026-01-01)
    #[arg(long)]
    start: Option<String>,
    /// End date (e.g. 2026-12-31)
    #[arg(long)]
    end: Option<String>,
    /// Granularity: hourly, daily, weekly, monthly, quarterly, semiAnnual, annual, total (case-insensitive, so MONTHLY etc. also work)
    #[arg(long)]
    granularity: Option<String>,
    /// Metrics to include (comma-separated; defaults to all available metrics)
    #[arg(long)]
    metrics: Option<String>,
    /// Dimension selections as JSON [{"name":"budget","select":"/","lod":1}] (defaults to all available dimensions at root, lod 1)
    #[arg(long, value_name = "JSON", value_parser = parse_dimensions)]
    dimensions: Option<Value>,
    /// Currency code
    #[arg(long)]
    currency: Option<String>,
}

pub async fn run(
    cmd: AnalyticsCommand,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    match cmd {
        AnalyticsCommand::Metrics => metrics(format, profile_name).await,
        AnalyticsCommand::Dimensions => dimensions(format, profile_name).await,
        AnalyticsCommand::Currencies => currencies(format, profile_name).await,
        AnalyticsCommand::Series(args) => series(args, format, profile_name).await,
    }
}

async fn metrics(format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    let data = client.query("{ analytics { metrics } }", None).await?;

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
        .query(
            "{ analytics { dimensions { name values { path label } } } }",
            None,
        )
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
                                .filter_map(|v| v["label"].as_str().or_else(|| v["path"].as_str()))
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

    let data = client.query("{ analytics { currencies } }", None).await?;

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

/// Map a user-supplied granularity to the server's `AnalyticsGranularity`
/// enum casing, case-insensitively (`MONTHLY` → `monthly`, `semiannual` /
/// `SEMI_ANNUAL` → `semiAnnual`). Legacy aliases from older help text
/// (`ANNUALLY`) are accepted too. Unknown values pass through unchanged so
/// the server can report the valid options.
fn normalize_granularity(input: &str) -> String {
    let key: String = input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    match key.as_str() {
        "hourly" => "hourly",
        "daily" => "daily",
        "weekly" => "weekly",
        "monthly" => "monthly",
        "quarterly" => "quarterly",
        "semiannual" | "semiannually" => "semiAnnual",
        "annual" | "annually" => "annual",
        "total" => "total",
        _ => return input.to_string(),
    }
    .to_string()
}

fn parse_dimensions(input: &str) -> std::result::Result<Value, String> {
    #[derive(serde::Deserialize, serde::Serialize)]
    #[serde(deny_unknown_fields)]
    struct Dimension {
        name: String,
        select: String,
        lod: i32,
    }
    let dimensions: Vec<Dimension> = serde_json::from_str(input)
        .map_err(|e| format!("expected JSON array of {{name, select, lod}} objects: {e}"))?;
    if dimensions.is_empty()
        || dimensions
            .iter()
            .any(|d| d.name.trim().is_empty() || d.select.trim().is_empty() || d.lod < 0)
    {
        return Err(
            "provide at least one dimension with nonempty name/select and nonnegative lod".into(),
        );
    }
    serde_json::to_value(dimensions).map_err(|e| e.to_string())
}

async fn series(args: SeriesArgs, format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    let metadata = if args.dimensions.is_none() || args.metrics.is_none() {
        client
            .query("{ analytics { metrics dimensions { name } } }", None)
            .await?
    } else {
        Value::Null
    };
    let dimensions = args.dimensions.unwrap_or_else(|| {
        let selections: Vec<Value> = metadata
            .pointer("/analytics/dimensions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|dimension| dimension["name"].as_str())
            .map(|name| serde_json::json!({"name": name, "select": "/", "lod": 1}))
            .collect();
        Value::Array(selections)
    });
    // The reactor requires a dimension even though its schema makes it optional.
    // With no indexed dimensions there is no series data to query.
    if dimensions.as_array().is_some_and(Vec::is_empty) {
        match format {
            OutputFormat::Json | OutputFormat::Raw => print_json(&serde_json::json!([])),
            _ => println!("No analytics data found."),
        }
        return Ok(());
    }
    let metrics = match args.metrics {
        Some(metrics) => serde_json::json!(metrics.split(',').map(str::trim).collect::<Vec<_>>()),
        None => metadata
            .pointer("/analytics/metrics")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
    };
    let mut filter = serde_json::json!({"dimensions": dimensions, "metrics": metrics});
    for (key, value) in [
        ("start", args.start),
        ("end", args.end),
        (
            "granularity",
            args.granularity.map(|g| normalize_granularity(&g)),
        ),
        ("currency", args.currency),
    ] {
        if let Some(value) = value {
            filter[key] = Value::String(value);
        }
    }
    let variables = serde_json::json!({"filter": filter});
    let query = "query($filter: AnalyticsFilter!) { analytics { series(filter: $filter) { period start end rows { metric value unit sum } } } }";
    let data = client.query(query, Some(&variables)).await?;

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

#[cfg(test)]
mod tests {
    use super::{normalize_granularity, parse_dimensions};

    #[test]
    fn dimension_selections_are_validated_before_dispatch() {
        assert!(parse_dimensions(r#"[{"name":"budget","select":"/","lod":1}]"#).is_ok());
        for input in [
            "[]",
            "{}",
            r#"[{"name":"budget","select":"/"}]"#,
            r#"[{"name":"","select":"/","lod":1}]"#,
            r#"[{"name":"budget","select":"/","lod":-1}]"#,
        ] {
            assert!(parse_dimensions(input).is_err(), "{input}");
        }
    }

    #[test]
    fn granularity_matches_the_server_enum() {
        assert_eq!(normalize_granularity("MONTHLY"), "monthly");
        assert_eq!(normalize_granularity("SEMI_ANNUAL"), "semiAnnual");
        assert_eq!(normalize_granularity("ANNUALLY"), "annual");
    }
}
