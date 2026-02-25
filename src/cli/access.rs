use anyhow::Result;
use clap::Subcommand;
use colored::Colorize;
use serde_json::Value;

use crate::cli::helpers;
use crate::output::{OutputFormat, print_json, print_table};

#[derive(Subcommand)]
pub enum AccessCommand {
    /// Show permissions for a document
    Show {
        /// Document ID
        document_id: String,
    },
    /// Grant permission on a document
    Grant {
        /// Document ID
        document_id: String,
        /// User address
        #[arg(long)]
        user: String,
        /// Permission level
        #[arg(long)]
        level: String,
    },
    /// Revoke permission on a document
    Revoke {
        /// Document ID
        document_id: String,
        /// User address
        #[arg(long)]
        user: String,
    },
    /// Grant group permission on a document
    GrantGroup {
        /// Document ID
        document_id: String,
        /// Group ID
        #[arg(long)]
        group: String,
        /// Permission level
        #[arg(long)]
        level: String,
    },
    /// Revoke group permission on a document
    RevokeGroup {
        /// Document ID
        document_id: String,
        /// Group ID
        #[arg(long)]
        group: String,
    },
    /// Manage operation-level permissions
    #[command(subcommand)]
    Ops(OpsCommand),
}

#[derive(Subcommand)]
pub enum OpsCommand {
    /// Show permissions for a specific operation type on a document
    Show {
        /// Document ID
        document_id: String,
        /// Operation type (e.g. "SET_NAME")
        operation_type: String,
    },
    /// Check if the current user can execute an operation
    CanExecute {
        /// Document ID
        document_id: String,
        /// Operation type
        operation_type: String,
    },
    /// Grant operation permission to a user
    Grant {
        /// Document ID
        document_id: String,
        /// Operation type
        operation_type: String,
        /// User address
        #[arg(long)]
        user: String,
    },
    /// Revoke operation permission from a user
    Revoke {
        /// Document ID
        document_id: String,
        /// Operation type
        operation_type: String,
        /// User address
        #[arg(long)]
        user: String,
    },
    /// Grant operation permission to a group
    GrantGroup {
        /// Document ID
        document_id: String,
        /// Operation type
        operation_type: String,
        /// Group ID
        #[arg(long)]
        group: String,
    },
    /// Revoke operation permission from a group
    RevokeGroup {
        /// Document ID
        document_id: String,
        /// Operation type
        operation_type: String,
        /// Group ID
        #[arg(long)]
        group: String,
    },
}

pub async fn run(cmd: AccessCommand, format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    match cmd {
        AccessCommand::Show { document_id } => show(&document_id, format, profile_name).await,
        AccessCommand::Grant { document_id, user, level } => {
            grant(&document_id, &user, &level, format, profile_name).await
        }
        AccessCommand::Revoke { document_id, user } => {
            revoke(&document_id, &user, format, profile_name).await
        }
        AccessCommand::GrantGroup { document_id, group, level } => {
            grant_group(&document_id, &group, &level, format, profile_name).await
        }
        AccessCommand::RevokeGroup { document_id, group } => {
            revoke_group(&document_id, &group, format, profile_name).await
        }
        AccessCommand::Ops(ops_cmd) => run_ops(ops_cmd, format, profile_name).await,
    }
}

async fn run_ops(cmd: OpsCommand, format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    match cmd {
        OpsCommand::Show { document_id, operation_type } => {
            ops_show(&document_id, &operation_type, format, profile_name).await
        }
        OpsCommand::CanExecute { document_id, operation_type } => {
            ops_can_execute(&document_id, &operation_type, format, profile_name).await
        }
        OpsCommand::Grant { document_id, operation_type, user } => {
            ops_grant(&document_id, &operation_type, &user, format, profile_name).await
        }
        OpsCommand::Revoke { document_id, operation_type, user } => {
            ops_revoke(&document_id, &operation_type, &user, format, profile_name).await
        }
        OpsCommand::GrantGroup { document_id, operation_type, group } => {
            ops_grant_group(&document_id, &operation_type, &group, format, profile_name).await
        }
        OpsCommand::RevokeGroup { document_id, operation_type, group } => {
            ops_revoke_group(&document_id, &operation_type, &group, format, profile_name).await
        }
    }
}

/// Build a client pointing at the auth subgraph
fn auth_url(base_url: &str) -> String {
    // Base URL is like https://host/graphql — auth subgraph is at /graphql/auth
    if base_url.ends_with("/graphql") {
        format!("{base_url}/auth")
    } else {
        format!("{base_url}/auth")
    }
}

async fn show(document_id: &str, format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    let (_name, profile, client) = helpers::setup(profile_name)?;

    let query = format!(
        r#"{{ documentAccess(documentId: "{id}") {{ userId permission }} }}"#,
        id = document_id.replace('"', r#"\""#)
    );

    // Try auth subgraph first, fall back to main endpoint
    let auth_client = crate::graphql::GraphQLClient::new(
        auth_url(&profile.url),
        profile.token.clone(),
    );

    let data = match auth_client.query(&query, None).await {
        Ok(d) => d,
        Err(_) => client.query(&query, None).await?,
    };

    let access = data
        .get("documentAccess")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&Value::Array(access)),
        OutputFormat::Table => {
            if access.is_empty() {
                println!("No permissions set for document '{document_id}'.");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = access
                .iter()
                .map(|a| {
                    vec![
                        a["userId"].as_str().unwrap_or("-").to_string(),
                        a["permission"].as_str().unwrap_or("-").to_string(),
                    ]
                })
                .collect();
            print_table(&["User", "Permission"], &rows);
        }
    }

    Ok(())
}

async fn grant(
    document_id: &str,
    user: &str,
    level: &str,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_name, profile, client) = helpers::setup(profile_name)?;

    let mutation = format!(
        r#"mutation {{ grantDocumentPermission(documentId: "{doc}", userAddress: "{user}", permission: {level}) }}"#,
        doc = document_id.replace('"', r#"\""#),
        user = user.replace('"', r#"\""#),
        level = level.to_uppercase(),
    );

    let auth_client = crate::graphql::GraphQLClient::new(
        auth_url(&profile.url),
        profile.token.clone(),
    );

    let data = match auth_client.query(&mutation, None).await {
        Ok(d) => d,
        Err(_) => client.query(&mutation, None).await?,
    };

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&data),
        OutputFormat::Table => {
            println!("{} Permission '{}' granted to {} on {}", "✓".green(), level, user, document_id);
        }
    }

    Ok(())
}

async fn revoke(
    document_id: &str,
    user: &str,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_name, profile, client) = helpers::setup(profile_name)?;

    let mutation = format!(
        r#"mutation {{ revokeDocumentPermission(documentId: "{doc}", userAddress: "{user}") }}"#,
        doc = document_id.replace('"', r#"\""#),
        user = user.replace('"', r#"\""#),
    );

    let auth_client = crate::graphql::GraphQLClient::new(
        auth_url(&profile.url),
        profile.token.clone(),
    );

    let data = match auth_client.query(&mutation, None).await {
        Ok(d) => d,
        Err(_) => client.query(&mutation, None).await?,
    };

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&data),
        OutputFormat::Table => {
            println!("{} Permission revoked for {} on {}", "✓".green(), user, document_id);
        }
    }

    Ok(())
}

async fn grant_group(
    document_id: &str,
    group: &str,
    level: &str,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_name, profile, client) = helpers::setup(profile_name)?;

    let mutation = format!(
        r#"mutation {{ grantGroupDocumentPermission(documentId: "{doc}", groupId: "{group}", permission: {level}) }}"#,
        doc = document_id.replace('"', r#"\""#),
        group = group.replace('"', r#"\""#),
        level = level.to_uppercase(),
    );

    let auth_client = crate::graphql::GraphQLClient::new(
        auth_url(&profile.url),
        profile.token.clone(),
    );

    let data = match auth_client.query(&mutation, None).await {
        Ok(d) => d,
        Err(_) => client.query(&mutation, None).await?,
    };

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&data),
        OutputFormat::Table => {
            println!("{} Permission '{}' granted to group {} on {}", "✓".green(), level, group, document_id);
        }
    }

    Ok(())
}

async fn revoke_group(
    document_id: &str,
    group: &str,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_name, profile, client) = helpers::setup(profile_name)?;

    let mutation = format!(
        r#"mutation {{ revokeGroupDocumentPermission(documentId: "{doc}", groupId: "{group}") }}"#,
        doc = document_id.replace('"', r#"\""#),
        group = group.replace('"', r#"\""#),
    );

    let auth_client = crate::graphql::GraphQLClient::new(
        auth_url(&profile.url),
        profile.token.clone(),
    );

    let data = match auth_client.query(&mutation, None).await {
        Ok(d) => d,
        Err(_) => client.query(&mutation, None).await?,
    };

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&data),
        OutputFormat::Table => {
            println!("{} Group permission revoked for group {} on {}", "✓".green(), group, document_id);
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Operation-level permissions
// ---------------------------------------------------------------------------

fn make_auth_client(profile: &crate::config::Profile) -> crate::graphql::GraphQLClient {
    crate::graphql::GraphQLClient::new(auth_url(&profile.url), profile.token.clone())
}

async fn query_auth_or_main(
    profile: &crate::config::Profile,
    client: &crate::graphql::GraphQLClient,
    query: &str,
) -> Result<Value> {
    let auth_client = make_auth_client(profile);
    match auth_client.query(query, None).await {
        Ok(d) => Ok(d),
        Err(_) => client.query(query, None).await,
    }
}

fn esc(s: &str) -> String {
    s.replace('"', r#"\""#)
}

async fn ops_show(
    document_id: &str,
    operation_type: &str,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_name, profile, client) = helpers::setup(profile_name)?;

    let query = format!(
        r#"{{ operationAccess(documentId: "{doc}", operationType: "{op}") {{ userId permission }} }}"#,
        doc = esc(document_id),
        op = esc(operation_type),
    );

    let data = query_auth_or_main(&profile, &client, &query).await?;

    let access = data
        .get("operationAccess")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&Value::Array(access)),
        OutputFormat::Table => {
            if access.is_empty() {
                println!("No operation permissions for '{}' on '{}'.", operation_type, document_id);
                return Ok(());
            }
            let rows: Vec<Vec<String>> = access
                .iter()
                .map(|a| {
                    vec![
                        a["userId"].as_str().unwrap_or("-").to_string(),
                        a["permission"].as_str().unwrap_or("-").to_string(),
                    ]
                })
                .collect();
            print_table(&["User", "Permission"], &rows);
        }
    }

    Ok(())
}

async fn ops_can_execute(
    document_id: &str,
    operation_type: &str,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_name, profile, client) = helpers::setup(profile_name)?;

    let query = format!(
        r#"{{ canExecuteOperation(documentId: "{doc}", operationType: "{op}") }}"#,
        doc = esc(document_id),
        op = esc(operation_type),
    );

    let data = query_auth_or_main(&profile, &client, &query).await?;

    let allowed = data
        .get("canExecuteOperation")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&data),
        OutputFormat::Table => {
            if allowed {
                println!("{} You can execute '{}' on '{}'", "✓".green(), operation_type, document_id);
            } else {
                println!("{} You cannot execute '{}' on '{}'", "✗".red(), operation_type, document_id);
            }
        }
    }

    Ok(())
}

async fn ops_grant(
    document_id: &str,
    operation_type: &str,
    user: &str,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_name, profile, client) = helpers::setup(profile_name)?;

    let mutation = format!(
        r#"mutation {{ grantOperationPermission(documentId: "{doc}", operationType: "{op}", userAddress: "{user}") }}"#,
        doc = esc(document_id),
        op = esc(operation_type),
        user = esc(user),
    );

    let data = query_auth_or_main(&profile, &client, &mutation).await?;

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&data),
        OutputFormat::Table => {
            println!(
                "{} Operation '{}' permission granted to {} on {}",
                "✓".green(), operation_type, user, document_id
            );
        }
    }

    Ok(())
}

async fn ops_revoke(
    document_id: &str,
    operation_type: &str,
    user: &str,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_name, profile, client) = helpers::setup(profile_name)?;

    let mutation = format!(
        r#"mutation {{ revokeOperationPermission(documentId: "{doc}", operationType: "{op}", userAddress: "{user}") }}"#,
        doc = esc(document_id),
        op = esc(operation_type),
        user = esc(user),
    );

    let data = query_auth_or_main(&profile, &client, &mutation).await?;

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&data),
        OutputFormat::Table => {
            println!(
                "{} Operation '{}' permission revoked for {} on {}",
                "✓".green(), operation_type, user, document_id
            );
        }
    }

    Ok(())
}

async fn ops_grant_group(
    document_id: &str,
    operation_type: &str,
    group: &str,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_name, profile, client) = helpers::setup(profile_name)?;

    let mutation = format!(
        r#"mutation {{ grantGroupOperationPermission(documentId: "{doc}", operationType: "{op}", groupId: "{group}") }}"#,
        doc = esc(document_id),
        op = esc(operation_type),
        group = esc(group),
    );

    let data = query_auth_or_main(&profile, &client, &mutation).await?;

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&data),
        OutputFormat::Table => {
            println!(
                "{} Operation '{}' permission granted to group {} on {}",
                "✓".green(), operation_type, group, document_id
            );
        }
    }

    Ok(())
}

async fn ops_revoke_group(
    document_id: &str,
    operation_type: &str,
    group: &str,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_name, profile, client) = helpers::setup(profile_name)?;

    let mutation = format!(
        r#"mutation {{ revokeGroupOperationPermission(documentId: "{doc}", operationType: "{op}", groupId: "{group}") }}"#,
        doc = esc(document_id),
        op = esc(operation_type),
        group = esc(group),
    );

    let data = query_auth_or_main(&profile, &client, &mutation).await?;

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&data),
        OutputFormat::Table => {
            println!(
                "{} Operation '{}' group permission revoked for group {} on {}",
                "✓".green(), operation_type, group, document_id
            );
        }
    }

    Ok(())
}
