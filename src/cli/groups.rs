use anyhow::Result;
use clap::Subcommand;
use colored::Colorize;
use serde_json::Value;

use crate::cli::helpers;
use crate::output::{OutputFormat, print_json, print_table};

#[derive(Subcommand)]
pub enum GroupsCommand {
    /// List all groups
    List,
    /// Get group details
    Get {
        /// Group ID
        id: String,
    },
    /// Create a new group
    Create {
        /// Group name
        #[arg(long)]
        name: String,
        /// Group description
        #[arg(long)]
        description: Option<String>,
    },
    /// Delete a group
    Delete {
        /// Group ID
        id: String,
        /// Skip confirmation
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Add a user to a group
    AddUser {
        /// Group ID
        group_id: String,
        /// User address
        #[arg(long)]
        user: String,
    },
    /// Remove a user from a group
    RemoveUser {
        /// Group ID
        group_id: String,
        /// User address
        #[arg(long)]
        user: String,
    },
    /// List groups for a user
    UserGroups {
        /// User address
        address: String,
    },
}

pub async fn run(
    cmd: GroupsCommand,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    match cmd {
        GroupsCommand::List => list(format, profile_name).await,
        GroupsCommand::Get { id } => get(&id, format, profile_name).await,
        GroupsCommand::Create { name, description } => {
            create(&name, description.as_deref(), format, profile_name).await
        }
        GroupsCommand::Delete { id, yes } => delete(&id, yes, profile_name).await,
        GroupsCommand::AddUser { group_id, user } => {
            add_user(&group_id, &user, format, profile_name).await
        }
        GroupsCommand::RemoveUser { group_id, user } => {
            remove_user(&group_id, &user, format, profile_name).await
        }
        GroupsCommand::UserGroups { address } => user_groups(&address, format, profile_name).await,
    }
}

/// Build a client pointing at the auth subgraph
fn auth_url(base_url: &str) -> String {
    format!("{base_url}/auth")
}

fn make_auth_client(profile: &crate::config::Profile) -> crate::graphql::GraphQLClient {
    crate::graphql::GraphQLClient::new(auth_url(&profile.url), profile.token.clone())
}

async fn list(format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    let (_name, profile, client) = helpers::setup(profile_name)?;
    let auth_client = make_auth_client(&profile);

    let query = r#"{ groups { id name description } }"#;

    let data = match auth_client.query(query, None).await {
        Ok(d) => d,
        Err(_) => client.query(query, None).await?,
    };

    let groups = data
        .get("groups")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&Value::Array(groups)),
        _ => {
            if groups.is_empty() {
                println!("No groups found.");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = groups
                .iter()
                .map(|g| {
                    vec![
                        g["id"].as_str().unwrap_or("-").to_string(),
                        g["name"].as_str().unwrap_or("-").to_string(),
                        g["description"].as_str().unwrap_or("").to_string(),
                    ]
                })
                .collect();
            print_table(&["ID", "Name", "Description"], &rows);
        }
    }

    Ok(())
}

async fn get(id: &str, format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    let (_name, profile, client) = helpers::setup(profile_name)?;
    let auth_client = make_auth_client(&profile);

    let query = format!(
        r#"{{ group(id: "{id}") {{ id name description members {{ userAddress }} }} }}"#,
        id = id.replace('"', r#"\""#)
    );

    let data = match auth_client.query(&query, None).await {
        Ok(d) => d,
        Err(_) => client.query(&query, None).await?,
    };

    let group = &data["group"];

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(group),
        _ => {
            println!("ID:          {}", group["id"].as_str().unwrap_or("-"));
            println!("Name:        {}", group["name"].as_str().unwrap_or("-"));
            println!(
                "Description: {}",
                group["description"].as_str().unwrap_or("")
            );

            if let Some(members) = group.get("members").and_then(|v| v.as_array()) {
                println!("\nMembers ({}):", members.len());
                for member in members {
                    println!("  {}", member["userAddress"].as_str().unwrap_or("-"));
                }
            }
        }
    }

    Ok(())
}

async fn create(
    name: &str,
    description: Option<&str>,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_pname, profile, client) = helpers::setup(profile_name)?;
    let auth_client = make_auth_client(&profile);

    let desc_arg = match description {
        Some(d) => format!(r#", description: "{d}""#, d = d.replace('"', r#"\""#)),
        None => String::new(),
    };

    let mutation = format!(
        r#"mutation {{ createGroup(name: "{name}"{desc_arg}) {{ id name description }} }}"#,
        name = name.replace('"', r#"\""#),
    );

    let data = match auth_client.query(&mutation, None).await {
        Ok(d) => d,
        Err(_) => client.query(&mutation, None).await?,
    };

    let group = &data["createGroup"];

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(group),
        _ => {
            println!("{} Group created", "✓".green());
            println!("  ID:   {}", group["id"].as_str().unwrap_or("-"));
            println!("  Name: {}", group["name"].as_str().unwrap_or("-"));
        }
    }

    Ok(())
}

async fn delete(id: &str, skip_confirm: bool, profile_name: Option<&str>) -> Result<()> {
    let (_name, profile, client) = helpers::setup(profile_name)?;
    let auth_client = make_auth_client(&profile);

    if !skip_confirm {
        let confirm = dialoguer::Confirm::new()
            .with_prompt(format!("Delete group {id}?"))
            .default(false)
            .interact()?;
        if !confirm {
            println!("Aborted.");
            return Ok(());
        }
    }

    let mutation = format!(
        r#"mutation {{ deleteGroup(id: "{id}") }}"#,
        id = id.replace('"', r#"\""#)
    );

    match auth_client.query(&mutation, None).await {
        Ok(_) => {}
        Err(_) => {
            client.query(&mutation, None).await?;
        }
    }

    println!("{} Group deleted.", "✓".green());
    Ok(())
}

async fn add_user(
    group_id: &str,
    user: &str,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_name, profile, client) = helpers::setup(profile_name)?;
    let auth_client = make_auth_client(&profile);

    let mutation = format!(
        r#"mutation {{ addUserToGroup(groupId: "{gid}", userAddress: "{user}") }}"#,
        gid = group_id.replace('"', r#"\""#),
        user = user.replace('"', r#"\""#),
    );

    let data = match auth_client.query(&mutation, None).await {
        Ok(d) => d,
        Err(_) => client.query(&mutation, None).await?,
    };

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&data),
        _ => {
            println!("{} User '{user}' added to group '{group_id}'", "✓".green());
        }
    }

    Ok(())
}

async fn remove_user(
    group_id: &str,
    user: &str,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_name, profile, client) = helpers::setup(profile_name)?;
    let auth_client = make_auth_client(&profile);

    let mutation = format!(
        r#"mutation {{ removeUserFromGroup(groupId: "{gid}", userAddress: "{user}") }}"#,
        gid = group_id.replace('"', r#"\""#),
        user = user.replace('"', r#"\""#),
    );

    let data = match auth_client.query(&mutation, None).await {
        Ok(d) => d,
        Err(_) => client.query(&mutation, None).await?,
    };

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&data),
        _ => {
            println!(
                "{} User '{user}' removed from group '{group_id}'",
                "✓".green()
            );
        }
    }

    Ok(())
}

async fn user_groups(
    address: &str,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_name, profile, client) = helpers::setup(profile_name)?;
    let auth_client = make_auth_client(&profile);

    let query = format!(
        r#"{{ userGroups(userAddress: "{addr}") {{ id name }} }}"#,
        addr = address.replace('"', r#"\""#)
    );

    let data = match auth_client.query(&query, None).await {
        Ok(d) => d,
        Err(_) => client.query(&query, None).await?,
    };

    let groups = data
        .get("userGroups")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&Value::Array(groups)),
        _ => {
            if groups.is_empty() {
                println!("No groups found for user '{address}'.");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = groups
                .iter()
                .map(|g| {
                    vec![
                        g["id"].as_str().unwrap_or("-").to_string(),
                        g["name"].as_str().unwrap_or("-").to_string(),
                    ]
                })
                .collect();
            print_table(&["ID", "Name"], &rows);
        }
    }

    Ok(())
}
