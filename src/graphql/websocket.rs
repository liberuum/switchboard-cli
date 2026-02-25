use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// Subscribe to a GraphQL subscription via the graphql-ws protocol.
///
/// Connects to `ws_url`, performs the graphql-ws handshake (connection_init),
/// sends the subscription query, and calls `on_data` for each received payload.
/// Runs until the server closes or Ctrl+C is pressed.
pub async fn subscribe<F>(
    ws_url: &str,
    token: Option<&str>,
    subscription: &str,
    on_data: F,
) -> Result<()>
where
    F: Fn(Value),
{
    let url = url::Url::parse(ws_url).context("Invalid WebSocket URL")?;

    let request = tokio_tungstenite::tungstenite::http::Request::builder()
        .uri(ws_url)
        .header("Sec-WebSocket-Protocol", "graphql-transport-ws")
        .header("Host", url.host_str().unwrap_or(""))
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header(
            "Sec-WebSocket-Key",
            tokio_tungstenite::tungstenite::handshake::client::generate_key(),
        )
        .body(())
        .context("Failed to build WebSocket request")?;

    let (ws_stream, _response) = connect_async(request)
        .await
        .with_context(|| format!("Failed to connect to {ws_url}"))?;

    let (mut write, mut read) = ws_stream.split();

    // Step 1: Send connection_init with optional auth payload
    let init_payload = match token {
        Some(t) => {
            json!({ "type": "connection_init", "payload": { "Authorization": format!("Bearer {t}") } })
        }
        None => json!({ "type": "connection_init" }),
    };
    write
        .send(Message::Text(init_payload.to_string().into()))
        .await
        .context("Failed to send connection_init")?;

    // Step 2: Wait for connection_ack
    let mut acked = false;
    while let Some(msg) = read.next().await {
        let msg = msg.context("WebSocket read error")?;
        if let Message::Text(text) = &msg {
            let val: Value = serde_json::from_str(text.as_ref()).unwrap_or_default();
            match val["type"].as_str() {
                Some("connection_ack") => {
                    acked = true;
                    break;
                }
                Some("connection_error") => {
                    bail!("WebSocket connection error: {}", val["payload"]);
                }
                _ => {} // Ignore other messages during handshake
            }
        }
    }

    if !acked {
        bail!("WebSocket closed before connection_ack");
    }

    // Step 3: Send the subscription
    let subscribe_msg = json!({
        "id": "1",
        "type": "subscribe",
        "payload": {
            "query": subscription
        }
    });
    write
        .send(Message::Text(subscribe_msg.to_string().into()))
        .await
        .context("Failed to send subscribe message")?;

    // Step 4: Process incoming messages
    while let Some(msg) = read.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                eprintln!("WebSocket error: {e}");
                break;
            }
        };

        match &msg {
            Message::Text(text) => {
                let val: Value = serde_json::from_str(text.as_ref()).unwrap_or_default();
                match val["type"].as_str() {
                    Some("next") => {
                        if let Some(data) = val.get("payload").and_then(|p| p.get("data")) {
                            on_data(data.clone());
                        }
                    }
                    Some("error") => {
                        let errors = val
                            .get("payload")
                            .map(|p| p.to_string())
                            .unwrap_or_default();
                        eprintln!("Subscription error: {errors}");
                    }
                    Some("complete") => {
                        break;
                    }
                    _ => {}
                }
            }
            Message::Close(_) => break,
            Message::Ping(data) => {
                let _ = write.send(Message::Pong(data.clone())).await;
            }
            _ => {}
        }
    }

    Ok(())
}
