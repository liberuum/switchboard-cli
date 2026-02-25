use serde_json::Value;

/// Pretty-print a JSON value to stdout
pub fn print_json(value: &Value) {
    match serde_json::to_string_pretty(value) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("Failed to format JSON: {e}"),
    }
}
