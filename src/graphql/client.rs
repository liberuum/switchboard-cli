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
                Err(e) if e.is_timeout() => {
                    return Err(anyhow::Error::new(e).context(timed_out_message(&self.url)));
                }
                Err(e) => {
                    return Err(
                        anyhow::Error::new(e).context(format!("Request to {} failed", self.url))
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
            if status == reqwest::StatusCode::GATEWAY_TIMEOUT
                || status == reqwest::StatusCode::REQUEST_TIMEOUT
            {
                return Err(
                    anyhow::Error::new(MaybeApplied(format!("HTTP {status}: {body}")))
                        .context(timed_out_message(&self.url)),
                );
            }
            // Validation errors come back non-2xx (HTTP 400 in yoga and Apollo).
            if let Some(messages) = graphql_error_messages(&body) {
                return Err(
                    anyhow::Error::new(ServerErrors(messages)).context(format!("HTTP {status}"))
                );
            }
            bail!("HTTP {status}: {body}");
        }

        let gql_response: GraphQLResponse = response
            .json()
            .await
            .context("Failed to parse GraphQL response")?;

        if let Some(errors) = gql_response.errors.filter(|e| !e.is_empty()) {
            let messages = errors.into_iter().map(|e| e.message).collect();
            return Err(anyhow::Error::new(ServerErrors(messages)));
        }

        gql_response.data.context("No data in GraphQL response")
    }

    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }
}

fn graphql_error_messages(body: &str) -> Option<Vec<String>> {
    let parsed: GraphQLResponse = serde_json::from_str(body).ok()?;
    let errors = parsed.errors.filter(|e| !e.is_empty())?;
    Some(errors.into_iter().map(|e| e.message).collect())
}

fn timed_out_message(url: &str) -> String {
    format!(
        "Timed out waiting for a response from {url}. \
         The request reached the server and the operation may have completed \
         — check before retrying."
    )
}

/// A gateway's 504/408: reached the server, no answer in time.
#[derive(Debug)]
pub struct MaybeApplied(String);

impl std::fmt::Display for MaybeApplied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for MaybeApplied {}

/// Errors the server answered with, as opposed to ones the transport produced.
#[derive(Debug)]
pub struct ServerErrors(pub Vec<String>);

impl std::fmt::Display for ServerErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GraphQL errors:\n  {}", self.0.join("\n  "))
    }
}

impl std::error::Error for ServerErrors {}

pub fn is_server_error(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|cause| cause.downcast_ref::<ServerErrors>().is_some())
}

/// True when the request reached the server but no answer came back in time,
/// so a mutation may have been applied anyway.
pub fn is_timeout_error(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause.downcast_ref::<MaybeApplied>().is_some()
            || cause
                .downcast_ref::<reqwest::Error>()
                .is_some_and(|e| e.is_timeout() && !e.is_connect())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_error_is_told_from_a_transport_one() {
        let err = anyhow::Error::new(ServerErrors(vec![
            "Cannot return null for non-nullable field JobInfo.result.".to_string(),
        ]))
        .context("jobStatus failed");
        assert!(is_server_error(&err));
        assert!(!is_timeout_error(&err));
        assert!(format!("{err:#}").contains("JobInfo.result"));

        let transport = anyhow::anyhow!("Failed to connect to http://localhost:4001/graphql");
        assert!(!is_server_error(&transport));
    }

    #[tokio::test]
    async fn timeout_survives_context_wrapping() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // Accept but never respond, so the request times out mid-response.
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            drop(stream);
        });

        let client = Client::builder()
            .timeout(std::time::Duration::from_millis(300))
            .build()
            .unwrap();
        let err = client
            .post(format!("http://{addr}/graphql"))
            .body("{}")
            .send()
            .await
            .unwrap_err();
        assert!(err.is_timeout());

        let wrapped = anyhow::Error::new(err).context("Timed out waiting for a response");
        assert!(is_timeout_error(&wrapped));
    }

    #[tokio::test]
    async fn connect_failure_is_not_a_timeout() {
        // Bind then drop, so the port is free and the connect is refused.
        let addr = {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap()
        };

        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let err = client
            .post(format!("http://{addr}/graphql"))
            .body("{}")
            .send()
            .await
            .unwrap_err();

        let wrapped = anyhow::Error::new(err).context("Failed to connect");
        assert!(!is_timeout_error(&wrapped));
    }

    // A gateway timeout is a successful HTTP exchange: no `reqwest::Error` in the chain.
    #[test]
    fn a_gateway_timeout_is_a_maybe_applied_timeout() {
        let err = anyhow::Error::new(MaybeApplied("HTTP 504 Gateway Timeout: ".to_string()))
            .context(timed_out_message("http://localhost:4001/graphql"));
        assert!(is_timeout_error(&err));
        assert!(!is_server_error(&err));
        assert!(format!("{err:#}").contains("may have completed"));
    }

    #[test]
    fn a_non_2xx_graphql_body_is_a_server_error() {
        let body =
            r#"{"errors":[{"message":"Cannot query field \"result\" on type \"JobInfo\"."}]}"#;
        let messages = graphql_error_messages(body).expect("a GraphQL error body");
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("JobInfo"));

        assert!(graphql_error_messages("<html>502 Bad Gateway</html>").is_none());
        assert!(graphql_error_messages(r#"{"data":{"x":1}}"#).is_none());
    }
}
