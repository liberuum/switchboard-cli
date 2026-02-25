use anyhow::Result;

use crate::cli::helpers;
use crate::output::{OutputFormat, print_json};

const FULL_SCHEMA_QUERY: &str = r#"{
  __schema {
    queryType { name }
    mutationType { name }
    subscriptionType { name }
    types {
      kind name description
      fields(includeDeprecated: true) {
        name description isDeprecated deprecationReason
        args { name description type { ...TypeRef } defaultValue }
        type { ...TypeRef }
      }
      inputFields {
        name description type { ...TypeRef } defaultValue
      }
      interfaces { ...TypeRef }
      enumValues(includeDeprecated: true) {
        name description isDeprecated deprecationReason
      }
      possibleTypes { ...TypeRef }
    }
    directives {
      name description locations
      args { name description type { ...TypeRef } defaultValue }
    }
  }
}

fragment TypeRef on __Type {
  kind name
  ofType {
    kind name
    ofType {
      kind name
      ofType {
        kind name
        ofType {
          kind name
        }
      }
    }
  }
}"#;

pub async fn run(format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    let data = client.query(FULL_SCHEMA_QUERY, None).await?;

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&data),
        OutputFormat::Table => {
            // For table mode, show a summary instead of the massive schema
            if let Some(types) = data.pointer("/__schema/types").and_then(|v| v.as_array()) {
                let user_types: Vec<_> = types
                    .iter()
                    .filter(|t| {
                        let name = t["name"].as_str().unwrap_or("");
                        !name.starts_with("__")
                    })
                    .collect();

                let objects = user_types
                    .iter()
                    .filter(|t| t["kind"].as_str() == Some("OBJECT"))
                    .count();
                let inputs = user_types
                    .iter()
                    .filter(|t| t["kind"].as_str() == Some("INPUT_OBJECT"))
                    .count();
                let enums = user_types
                    .iter()
                    .filter(|t| t["kind"].as_str() == Some("ENUM"))
                    .count();
                let scalars = user_types
                    .iter()
                    .filter(|t| t["kind"].as_str() == Some("SCALAR"))
                    .count();
                let unions = user_types
                    .iter()
                    .filter(|t| t["kind"].as_str() == Some("UNION"))
                    .count();
                let interfaces = user_types
                    .iter()
                    .filter(|t| t["kind"].as_str() == Some("INTERFACE"))
                    .count();

                println!("Schema Summary:");
                println!("  Objects:    {objects}");
                println!("  Inputs:     {inputs}");
                println!("  Enums:      {enums}");
                println!("  Scalars:    {scalars}");
                println!("  Unions:     {unions}");
                println!("  Interfaces: {interfaces}");
                println!();
                println!("Use --format json for full schema output.");
            }
        }
    }

    Ok(())
}
