use std::collections::HashMap;

use anyhow::{Result, bail};
use colored::Colorize;
use dialoguer::{Confirm, Input, MultiSelect, Select};
use serde_json::{Map, Value};

use crate::graphql::GraphQLClient;

// ── Types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct InputField {
    pub name: String,
    pub field_type: FieldType,
    pub required: bool,
}

#[derive(Debug, Clone)]
pub enum FieldType {
    Scalar(String),
    Enum(Vec<String>),
    InputObject(Vec<InputField>),
    List(Box<FieldType>),
}

impl FieldType {
    fn display(&self) -> String {
        match self {
            FieldType::Scalar(s) => s.clone(),
            FieldType::Enum(vals) => format!("Enum({})", vals.join(" | ")),
            FieldType::InputObject(_) => "Object".to_string(),
            FieldType::List(inner) => format!("[{}]", inner.display()),
        }
    }
}

// ── Schema introspection ─────────────────────────────────────────────

/// Fetch the fields of a GraphQL input type via __type introspection.
pub async fn fetch_input_type_schema(
    client: &GraphQLClient,
    type_name: &str,
) -> Result<Vec<InputField>> {
    let mut type_cache: HashMap<String, Vec<InputField>> = HashMap::new();
    fetch_type_recursive(client, type_name, &mut type_cache, 0).await
}

async fn fetch_type_recursive(
    client: &GraphQLClient,
    type_name: &str,
    cache: &mut HashMap<String, Vec<InputField>>,
    depth: usize,
) -> Result<Vec<InputField>> {
    if depth > 5 {
        bail!("Input type nesting too deep (>5 levels)");
    }

    if let Some(cached) = cache.get(type_name) {
        return Ok(cached.clone());
    }

    let query = format!(
        r#"{{ __type(name: "{type_name}") {{
            kind
            inputFields {{
                name
                type {{ name kind ofType {{ name kind ofType {{ name kind ofType {{ name kind }} }} }} }}
            }}
            enumValues {{ name }}
        }} }}"#,
    );

    let data = client.query(&query, None).await?;
    let type_info = data
        .get("__type")
        .filter(|v| !v.is_null())
        .ok_or_else(|| anyhow::anyhow!("Type '{type_name}' not found in schema"))?;

    let kind = type_info["kind"].as_str().unwrap_or("");

    if kind == "ENUM" {
        return Ok(vec![]);
    }

    let input_fields = type_info["inputFields"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Type '{type_name}' has no inputFields"))?;

    let mut fields = Vec::new();

    for field in input_fields {
        let name = field["name"].as_str().unwrap_or_default().to_string();
        let (field_type, required) =
            resolve_field_type(client, &field["type"], cache, depth).await?;
        fields.push(InputField {
            name,
            field_type,
            required,
        });
    }

    cache.insert(type_name.to_string(), fields.clone());
    Ok(fields)
}

type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

fn resolve_field_type<'a>(
    client: &'a GraphQLClient,
    type_val: &'a Value,
    cache: &'a mut HashMap<String, Vec<InputField>>,
    depth: usize,
) -> BoxFuture<'a, Result<(FieldType, bool)>> {
    Box::pin(async move {
        let kind = type_val["kind"].as_str().unwrap_or("");
        let name = type_val["name"].as_str().unwrap_or("");

        match kind {
            "NON_NULL" => {
                let (ft, _) = resolve_field_type(client, &type_val["ofType"], cache, depth).await?;
                Ok((ft, true))
            }
            "LIST" => {
                let (inner, _) =
                    resolve_field_type(client, &type_val["ofType"], cache, depth).await?;
                Ok((FieldType::List(Box::new(inner)), false))
            }
            "INPUT_OBJECT" => {
                let sub_fields = fetch_type_recursive(client, name, cache, depth + 1).await?;
                Ok((FieldType::InputObject(sub_fields), false))
            }
            "ENUM" => {
                let enum_vals = fetch_enum_values(client, name).await?;
                Ok((FieldType::Enum(enum_vals), false))
            }
            _ => Ok((FieldType::Scalar(name.to_string()), false)),
        }
    })
}

async fn fetch_enum_values(client: &GraphQLClient, type_name: &str) -> Result<Vec<String>> {
    let query = format!(r#"{{ __type(name: "{type_name}") {{ enumValues {{ name }} }} }}"#,);
    let data = client.query(&query, None).await?;
    let values = data
        .pointer("/__type/enumValues")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v["name"].as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    Ok(values)
}

// ── Document state ───────────────────────────────────────────────────

/// Fetch the current document state via document(identifier).
pub async fn fetch_document_state(
    client: &GraphQLClient,
    identifier: &str,
) -> Result<Value> {
    let query = format!(
        r#"{{ document(identifier: "{id}") {{ document {{ state }} }} }}"#,
        id = identifier.replace('"', r#"\""#),
    );

    let data = client.query(&query, None).await?;
    match data.pointer("/document/document/state") {
        Some(val) if val.is_object() => Ok(val.clone()),
        _ => Ok(Value::Object(Map::new())),
    }
}

// ── Display current state ────────────────────────────────────────────

/// Show all fields with their current values and types.
fn print_field_summary(fields: &[InputField], state: &Value) {
    println!("\n{}", "Available fields:".bold());
    for field in fields {
        let current = state.get(&field.name);
        let type_str = field.field_type.display().dimmed();
        let val_str = format_current_value(current);
        println!(
            "  {} {} = {} {}",
            "·".dimmed(),
            field.name,
            val_str,
            type_str
        );
    }
    println!();
}

fn format_current_value(val: Option<&Value>) -> String {
    match val {
        None | Some(Value::Null) => "(empty)".dimmed().to_string(),
        Some(Value::String(s)) if s.is_empty() => "(empty)".dimmed().to_string(),
        Some(Value::String(s)) => {
            if s.len() > 50 {
                format!("\"{}...\"", &s[..47]).green().to_string()
            } else {
                format!("\"{s}\"").green().to_string()
            }
        }
        Some(Value::Bool(b)) => b.to_string().green().to_string(),
        Some(Value::Number(n)) => n.to_string().green().to_string(),
        Some(Value::Array(arr)) => {
            if arr.is_empty() {
                "[]".dimmed().to_string()
            } else {
                format!("[{} items]", arr.len()).green().to_string()
            }
        }
        Some(Value::Object(m)) => {
            if m.is_empty() {
                "{}".dimmed().to_string()
            } else {
                let keys: Vec<&String> = m.keys().take(3).collect();
                format!(
                    "{{ {} }}",
                    keys.iter()
                        .map(|k| k.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
                .green()
                .to_string()
            }
        }
    }
}

// ── Field selection + prompting ──────────────────────────────────────

/// Show fields with current state, let user pick which to edit, then prompt.
/// Returns a JSON object containing only the changed fields.
pub fn select_and_prompt_fields(fields: &[InputField], state: &Value) -> Result<Value> {
    // 1. Show current state overview
    print_field_summary(fields, state);

    // 2. Build selection list with display labels
    let labels: Vec<String> = fields
        .iter()
        .map(|f| {
            let current = state.get(&f.name);
            let val_preview = format_current_value(current);
            format!("{} = {}", f.name, val_preview)
        })
        .collect();

    let selected = MultiSelect::new()
        .with_prompt("Select fields to edit (Space to toggle, Enter to confirm)")
        .items(&labels)
        .interact()?;

    if selected.is_empty() {
        return Ok(Value::Object(Map::new()));
    }

    // 3. Prompt only selected fields
    println!();
    let mut result = Map::new();

    for &idx in &selected {
        let field = &fields[idx];
        let current = state.get(&field.name);

        let value = prompt_field(&field.name, &field.field_type, current, field.required)?;
        if let Some(v) = value {
            result.insert(field.name.clone(), v);
        }
    }

    Ok(Value::Object(result))
}

fn prompt_field(
    label: &str,
    field_type: &FieldType,
    current: Option<&Value>,
    required: bool,
) -> Result<Option<Value>> {
    match field_type {
        FieldType::Scalar(scalar) => prompt_scalar(label, scalar, current, required),
        FieldType::Enum(values) => prompt_enum(label, values, current),
        FieldType::InputObject(sub_fields) => {
            let sub_state = current.cloned().unwrap_or(Value::Object(Map::new()));
            println!("  {} {}", "▸".dimmed(), label.bold());
            let obj = select_and_prompt_fields(sub_fields, &sub_state)?;
            if obj.as_object().map(|m| m.is_empty()).unwrap_or(true) {
                Ok(None)
            } else {
                Ok(Some(obj))
            }
        }
        FieldType::List(inner) => prompt_list(label, inner, current),
    }
}

fn prompt_scalar(
    label: &str,
    scalar: &str,
    current: Option<&Value>,
    required: bool,
) -> Result<Option<Value>> {
    match scalar {
        "Boolean" => {
            let default = current.and_then(|v| v.as_bool()).unwrap_or(false);
            let val = Confirm::new()
                .with_prompt(label)
                .default(default)
                .interact()?;
            Ok(Some(Value::Bool(val)))
        }
        "Int" => {
            let default = current
                .and_then(|v| v.as_i64())
                .map(|n| n.to_string())
                .unwrap_or_default();
            loop {
                let input: String = if default.is_empty() && !required {
                    Input::new()
                        .with_prompt(format!("{label} (Int)"))
                        .allow_empty(true)
                        .interact_text()?
                } else {
                    Input::new()
                        .with_prompt(format!("{label} (Int)"))
                        .default(default.clone())
                        .interact_text()?
                };
                if input.is_empty() && !required {
                    return Ok(None);
                }
                match input.parse::<i64>() {
                    Ok(n) => return Ok(Some(Value::Number(n.into()))),
                    Err(_) => {
                        eprintln!("{} Expected an integer, got \"{input}\"", "!".red());
                    }
                }
            }
        }
        "Float" => {
            let default = current
                .and_then(|v| v.as_f64())
                .map(|n| n.to_string())
                .unwrap_or_default();
            loop {
                let input: String = if default.is_empty() && !required {
                    Input::new()
                        .with_prompt(format!("{label} (Float)"))
                        .allow_empty(true)
                        .interact_text()?
                } else {
                    Input::new()
                        .with_prompt(format!("{label} (Float)"))
                        .default(default.clone())
                        .interact_text()?
                };
                if input.is_empty() && !required {
                    return Ok(None);
                }
                match input.parse::<f64>() {
                    Ok(n) => return Ok(Some(serde_json::json!(n))),
                    Err(_) => {
                        eprintln!("{} Expected a number, got \"{input}\"", "!".red());
                    }
                }
            }
        }
        _ => {
            // String, ID, DateTime, URL, and other custom scalars
            let default = current
                .filter(|v| !v.is_null())
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let type_hint = if scalar != "String" {
                format!(" ({scalar})")
            } else {
                String::new()
            };

            let input: String = if default.is_empty() && !required {
                Input::new()
                    .with_prompt(format!("{label}{type_hint}"))
                    .allow_empty(true)
                    .interact_text()?
            } else if default.is_empty() {
                Input::new()
                    .with_prompt(format!("{label}{type_hint}"))
                    .interact_text()?
            } else {
                Input::new()
                    .with_prompt(format!("{label}{type_hint}"))
                    .default(default)
                    .interact_text()?
            };

            if input.is_empty() && !required {
                Ok(None)
            } else {
                Ok(Some(Value::String(input)))
            }
        }
    }
}

fn prompt_enum(label: &str, values: &[String], current: Option<&Value>) -> Result<Option<Value>> {
    let current_str = current.and_then(|v| v.as_str()).unwrap_or("");
    let default_idx = values.iter().position(|v| v == current_str).unwrap_or(0);

    let selection = Select::new()
        .with_prompt(label)
        .items(values)
        .default(default_idx)
        .interact()?;

    Ok(Some(Value::String(values[selection].clone())))
}

fn prompt_list(
    label: &str,
    inner_type: &FieldType,
    current: Option<&Value>,
) -> Result<Option<Value>> {
    let current_arr = current
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let preview = if current_arr.is_empty() {
        "(empty)".to_string()
    } else {
        format!("{} items", current_arr.len())
    };

    println!("  {} {} [{}]", "▸".dimmed(), label.bold(), preview.dimmed());

    // Show existing items if any
    if !current_arr.is_empty() {
        for (i, item) in current_arr.iter().enumerate() {
            println!("    [{}] {}", i, value_preview(item));
        }
    }

    let choices = if current_arr.is_empty() {
        vec!["Keep empty", "Add item"]
    } else {
        vec![
            "Keep current",
            "Add item",
            "Remove item",
            "Replace all",
            "Clear",
        ]
    };

    let action = Select::new()
        .with_prompt(format!("{label} action"))
        .items(&choices)
        .default(0)
        .interact()?;

    match choices[action] {
        "Keep empty" | "Keep current" => Ok(None),
        "Add item" => {
            let mut items = current_arr;
            loop {
                let val = prompt_list_item(label, inner_type)?;
                items.push(val);
                if !Confirm::new()
                    .with_prompt("Add another?")
                    .default(false)
                    .interact()?
                {
                    break;
                }
            }
            Ok(Some(Value::Array(items)))
        }
        "Remove item" => {
            let mut items = current_arr;
            let display: Vec<String> = items
                .iter()
                .enumerate()
                .map(|(i, v)| format!("[{i}] {}", value_preview(v)))
                .collect();
            let sel = Select::new()
                .with_prompt("Remove which item?")
                .items(&display)
                .interact()?;
            items.remove(sel);
            Ok(Some(Value::Array(items)))
        }
        "Replace all" => {
            let input: String = Input::new()
                .with_prompt(format!("{label} (JSON array)"))
                .interact_text()?;
            let arr: Value = serde_json::from_str(&input)
                .map_err(|e| anyhow::anyhow!("Invalid JSON array: {e}"))?;
            if !arr.is_array() {
                bail!("Expected a JSON array");
            }
            Ok(Some(arr))
        }
        "Clear" => Ok(Some(Value::Array(vec![]))),
        _ => Ok(None),
    }
}

fn prompt_list_item(label: &str, inner_type: &FieldType) -> Result<Value> {
    match inner_type {
        FieldType::Scalar(s) => {
            let val = prompt_scalar(&format!("{label} item"), s, None, true)?;
            Ok(val.unwrap_or(Value::Null))
        }
        FieldType::Enum(values) => {
            let val = prompt_enum(&format!("{label} item"), values, None)?;
            Ok(val.unwrap_or(Value::Null))
        }
        FieldType::InputObject(fields) => {
            println!("  New item:");
            let obj = select_and_prompt_fields(fields, &Value::Object(Map::new()))?;
            Ok(obj)
        }
        FieldType::List(_) => {
            // Nested list — fall back to raw JSON
            let input: String = Input::new()
                .with_prompt(format!("{label} item (JSON)"))
                .interact_text()?;
            let val: Value = serde_json::from_str(&input)?;
            Ok(val)
        }
    }
}

fn value_preview(v: &Value) -> String {
    match v {
        Value::String(s) => {
            if s.len() > 50 {
                format!("\"{}...\"", &s[..47])
            } else {
                format!("\"{s}\"")
            }
        }
        Value::Object(m) => {
            let pairs: Vec<String> = m
                .iter()
                .take(3)
                .map(|(k, v)| {
                    let short = match v {
                        Value::String(s) => format!("\"{s}\""),
                        other => other.to_string(),
                    };
                    format!("{k}: {short}")
                })
                .collect();
            let suffix = if m.len() > 3 { ", ..." } else { "" };
            format!("{{ {}{suffix} }}", pairs.join(", "))
        }
        other => {
            let s = other.to_string();
            if s.len() > 50 {
                format!("{}...", &s[..47])
            } else {
                s
            }
        }
    }
}

// ── Confirmation ─────────────────────────────────────────────────────

/// Show a preview of the input and ask for confirmation.
pub fn confirm_input(input: &Value) -> Result<bool> {
    let pretty = serde_json::to_string_pretty(input)?;
    println!("\n{}", "Mutation input:".bold());
    println!("{pretty}");
    println!();

    let proceed = Confirm::new()
        .with_prompt("Apply mutation?")
        .default(true)
        .interact()?;

    Ok(proceed)
}
