use anyhow::Result;
use colored::Colorize;

use crate::cli::helpers;
use crate::graphql::introspection::{run_introspection, save_cache};

pub async fn run(profile_name: Option<&str>, quiet: bool) -> Result<()> {
    let (name, _profile, client) = helpers::setup(profile_name)?;

    if !quiet {
        println!("Introspecting {}...", client.url);
    }
    let cache = run_introspection(&client).await?;

    let model_count = cache.models.len();
    save_cache(&name, &cache)?;

    if !quiet {
        println!(
            "{} {model_count} document models discovered and cached.",
            "✓".green()
        );

        // Show a summary
        for (doc_type, model) in &cache.models {
            let op_count = model.operations.len().saturating_sub(1); // exclude createDocument
            println!("  {} ({} operations)", doc_type, op_count);
        }
    }

    Ok(())
}
