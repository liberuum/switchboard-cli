use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct GraphQLClient {
    client: Client,
    pub url: String,
    token: Option<String>,
}

#[derive(Debug, Serialize)]
struct GraphQLRequest<'a> {
    query: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    variables: Option<&'a Value>,
}

#[derive(Debug, Deserialize)]
struct GraphQLResponse {
    data: Option<Value>,
    errors: Option<Vec<GraphQLError>>,
}

#[derive(Debug, Deserialize)]
struct GraphQLError {
    message: String,
}

impl GraphQLClient {
    pub fn new(url: String, token: Option<String>) -> Self {
        // Check for env var override
        let token = std::env::var("SWITCHBOARD_TOKEN").ok().or(token);

        Self {
            client: Client::new(),
            url,
            token,
        }
    }

    pub async fn query(&self, query: &str, variables: Option<&Value>) -> Result<Value> {
        let request = GraphQLRequest { query, variables };

        let mut builder = self.client.post(&self.url).json(&request);

        if let Some(ref token) = self.token {
            builder = builder.header("Authorization", format!("Bearer {token}"));
        }

        let response = builder
            .send()
            .await
            .with_context(|| format!("Failed to connect to {}", self.url))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            bail!("HTTP {status}: {body}");
        }

        let gql_response: GraphQLResponse = response
            .json()
            .await
            .context("Failed to parse GraphQL response")?;

        if let Some(errors) = gql_response.errors.filter(|e| !e.is_empty()) {
            let messages: Vec<_> = errors.iter().map(|e| e.message.as_str()).collect();
            bail!("GraphQL errors:\n  {}", messages.join("\n  "));
        }

        gql_response.data.context("No data in GraphQL response")
    }

    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }
}
