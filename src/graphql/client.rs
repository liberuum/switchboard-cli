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

        // One pooled client per process: connections are kept alive and
        // reused across every query in this invocation (incl. interactive
        // mode). connect_timeout keeps a wedged TLS handshake from burning
        // the full request timeout; tcp_keepalive + a long idle pool keep
        // the reused connection healthy on flaky remote gateways.
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .connect_timeout(std::time::Duration::from_secs(10))
            .tcp_keepalive(std::time::Duration::from_secs(30))
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .build()
            .expect("failed to build HTTP client");

        Self { client, url, token }
    }

    pub async fn query(&self, query: &str, variables: Option<&Value>) -> Result<Value> {
        let request = GraphQLRequest { query, variables };

        // Bounded retries, ONLY where the request provably never reached the
        // application: connect-phase failures and 502/503 (gateway couldn't
        // reach the upstream). Never retried: 504/read timeouts (the server
        // may have executed the request — retrying could double-apply a
        // mutation) and anything with a GraphQL-level response.
        let mut last_err: Option<anyhow::Error> = None;
        let mut response = None;
        for attempt in 0..3u32 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(
                    400 * 2u64.pow(attempt - 1),
                ))
                .await;
            }
            let mut builder = self.client.post(&self.url).json(&request);
            if let Some(ref token) = self.token {
                builder = builder.header("Authorization", format!("Bearer {token}"));
            }
            match builder.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status == reqwest::StatusCode::BAD_GATEWAY
                        || status == reqwest::StatusCode::SERVICE_UNAVAILABLE
                    {
                        last_err = Some(anyhow::anyhow!("HTTP {status} (transient gateway error)"));
                        continue;
                    }
                    response = Some(resp);
                    break;
                }
                Err(e) if e.is_connect() => {
                    last_err = Some(
                        anyhow::Error::new(e).context(format!("Failed to connect to {}", self.url)),
                    );
                    continue;
                }
                Err(e) => {
                    return Err(
                        anyhow::Error::new(e).context(format!("Failed to connect to {}", self.url))
                    );
                }
            }
        }
        let response = match response {
            Some(r) => r,
            None => return Err(last_err.unwrap_or_else(|| anyhow::anyhow!("request failed"))),
        };

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
