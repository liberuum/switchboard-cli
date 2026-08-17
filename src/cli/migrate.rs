use anyhow::{Result, bail};
use colored::Colorize;
use serde_json::Value;
use std::io::Write;

use crate::cli::helpers;
use crate::cli::import_export::{fetch_drive_nodes, wait_for_job};
use crate::graphql::GraphQLClient;

/// In-place rolling progress reporter for the replay phase. Counts are bumped
/// after each successful `mutateDocumentAsync` batch and re-rendered onto the
/// same TTY line via `\r` + ANSI clear-to-end-of-line.
struct Progress {
    batches_done: usize,
    total_batches: usize,
    ops_done: usize,
    total_ops: usize,
    quiet: bool,
}

impl Progress {
    fn render(&self) {
        if self.quiet {
            return;
        }
        // \x1b[2K clears the entire line so a shorter render can't leave
        // trailing characters from a longer previous render.
        print!(
            "\r\x1b[2K  Replaying… batches {}/{} · ops {}/{}",
            self.batches_done, self.total_batches, self.ops_done, self.total_ops,
        );
        let _ = std::io::stdout().flush();
    }

    fn bump(&mut self, ops_in_batch: usize) {
        self.batches_done += 1;
        self.ops_done += ops_in_batch;
        self.render();
    }

    fn finish(&self) {
        if !self.quiet {
            // Newline so subsequent output starts cleanly below the progress line.
            println!();
        }
    }
}

/// Per-job wait budget for replays. Generous because a full drive may carry
/// hundreds of ops in a single mutateDocumentAsync batch.
const REPLAY_JOB_TIMEOUT_MS: u64 = 120_000;

/// Page size for `documentOperations` pagination. Matches the constant in
/// `import_export.rs` (which is module-private).
const OP_BATCH_SIZE: usize = 500;

pub async fn run(source_drive: String, from: String, to: String, quiet: bool) -> Result<()> {
    // Build both clients explicitly via the named profiles. setup() already
    // bails with a friendly message when a profile name is unknown, so we
    // get early validation of both arguments before any network I/O.
    let (from_name, _from_profile, src) = helpers::setup(Some(&from))?;
    let (to_name, _to_profile, dst) = helpers::setup(Some(&to))?;

    // Resolve the source drive identifier (slug or UUID) to its canonical UUID.
    let src_drive_id = helpers::resolve_doc(&src, &source_drive).await?;

    // Pull just the fields we actually need for the drive (name + slug).
    // We deliberately do NOT request `createdAtUtcIso` / `lastModifiedAtUtcIso`
    // here: those are declared non-nullable in the schema but sometimes come
    // back null in real data, which makes the broader `fetch_document` query
    // fail with "Cannot return null for non-nullable field" on drives that
    // would otherwise migrate fine.
    let (drive_name, drive_slug) = fetch_drive_label(&src, &src_drive_id).await?;

    // Replay needs the drive's full op history — CREATE_DOCUMENT (document
    // scope) plus every drive-scope op (SET_DRIVE_NAME, ADD_FILE, ADD_FOLDER,
    // …). `fetch_operations` skips metadata entirely so the null-field bug
    // above can't take the migration down.
    let src_drive_ops = fetch_operations(&src, &src_drive_id).await?;

    // Refuse to clobber: if any drive on the destination already carries this
    // slug, abort before sending a single action. We scan findDocuments rather
    // than relying on a single getter so the check matches what
    // `drives list` would show.
    if !drive_slug.is_empty() {
        let drives_data = dst
            .query(
                r#"{ findDocuments(search: { type: "powerhouse/document-drive" }) { items { id slug } } }"#,
                None,
            )
            .await?;
        let collision = drives_data
            .pointer("/findDocuments/items")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .any(|d| d.get("slug").and_then(|v| v.as_str()) == Some(drive_slug.as_str()))
            })
            .unwrap_or(false);
        if collision {
            bail!(
                "Destination profile '{to_name}' already has a drive with slug '{drive_slug}'. \
                 Refusing to overwrite — delete or rename it first."
            );
        }
    }

    if !quiet {
        println!(
            "  Migrating drive {} ({})",
            drive_name.green(),
            src_drive_id
        );
        println!("    {from_name}  →  {to_name}");
        println!("    Drive ops: {}", src_drive_ops.len());
    }

    // Discover the documents that live inside the drive, then pull each one's
    // full op history. We do this *before* any writes so a fetch failure
    // aborts the migration cleanly with the destination still untouched.
    let nodes = fetch_drive_nodes(&src, &src_drive_id).await?;
    let file_nodes: Vec<&Value> = nodes
        .iter()
        .filter(|n| n["kind"].as_str() == Some("file"))
        .collect();

    let mut docs: Vec<(String, String, Vec<Value>)> = Vec::with_capacity(file_nodes.len());
    for node in &file_nodes {
        let file_id = node["id"].as_str().unwrap_or("");
        let file_name = node["name"].as_str().unwrap_or("document").to_string();
        if file_id.is_empty() {
            continue;
        }
        let ops = fetch_operations(&src, file_id).await?;
        if !quiet {
            println!("    └ {} ({} ops)", file_name, ops.len());
        }
        docs.push((file_id.to_string(), file_name, ops));
    }

    // Pre-compute the work envelope so progress can show "X/Y batches" with
    // a stable denominator. Each distinct action.scope inside a doc becomes
    // one batch (mutateDocumentAsync rejects mixed-scope batches).
    let drive_batches = count_scopes(&src_drive_ops);
    let drive_total_ops = src_drive_ops.len();
    let child_batches: usize = docs.iter().map(|(_, _, ops)| count_scopes(ops)).sum();
    let child_total_ops: usize = docs.iter().map(|(_, _, ops)| ops.len()).sum();

    let mut progress = Progress {
        batches_done: 0,
        total_batches: drive_batches + child_batches,
        ops_done: 0,
        total_ops: drive_total_ops + child_total_ops,
        quiet,
    };
    progress.render();

    // Replay the drive document first — its CREATE_DOCUMENT op materialises
    // the destination drive with the source UUID, and the rest of the ops
    // restore name/icon/folder-tree state.
    let drive_ops_count = replay_document(
        &dst,
        &src_drive_id,
        &drive_name,
        &src_drive_ops,
        &mut progress,
    )
    .await?;

    // Then replay every contained document in the same order the drive lists
    // them. Each doc carries its own CREATE_DOCUMENT op, so passing its source
    // UUID as documentIdentifier preserves the ID end-to-end.
    let mut doc_ops_count = 0usize;
    for (doc_id, doc_name, ops) in &docs {
        doc_ops_count += replay_document(&dst, doc_id, doc_name, ops, &mut progress).await?;
    }
    progress.finish();

    if !quiet {
        let total_docs = 1 + docs.len();
        let total_ops = drive_ops_count + doc_ops_count;
        println!();
        println!("{} Migration complete", "✓".green());
        println!("    Drive:      {} ({})", drive_name, src_drive_id);
        println!(
            "    Slug:       {}",
            if drive_slug.is_empty() {
                "—"
            } else {
                &drive_slug
            }
        );
        println!(
            "    Documents:  {total_docs}  (1 drive + {} children)",
            docs.len()
        );
        println!("    Operations: {total_ops}");
    }

    Ok(())
}

/// Replay every op of a single document onto the destination, one batch per
/// scope. `mutateDocumentAsync` rejects mixed-scope batches with "All actions
/// must share the same scope", so we group ops by their `action.scope` and
/// submit each group separately. Document scope goes first because it carries
/// `CREATE_DOCUMENT` — the destination doc must exist before any other scope
/// can be appended to it. Within each scope, ops are submitted in `index`
/// order. Returns the total number of ops submitted across all scopes.
async fn replay_document(
    client: &GraphQLClient,
    doc_id: &str,
    doc_name: &str,
    operations: &[Value],
    progress: &mut Progress,
) -> Result<usize> {
    if operations.is_empty() {
        // A document with zero recorded ops is degenerate (it would not even
        // exist) — treat as a hard error so the migration's "1:1 fidelity"
        // contract is never silently violated.
        bail!(
            "Document '{doc_name}' ({doc_id}) has no operations to replay — \
             cannot reproduce on destination"
        );
    }

    // Group ops by scope, sorted by index within each group.
    use std::collections::HashMap;
    let mut by_scope: HashMap<String, Vec<&Value>> = HashMap::new();
    for op in operations {
        let scope = op
            .pointer("/action/scope")
            .and_then(|v| v.as_str())
            .unwrap_or("global")
            .to_string();
        by_scope.entry(scope).or_default().push(op);
    }
    for batch in by_scope.values_mut() {
        batch.sort_by_key(|op| op.get("index").and_then(|v| v.as_u64()).unwrap_or(0));
    }

    // Submit document scope first (CREATE_DOCUMENT materialises the doc),
    // then every other scope. The non-document order doesn't matter for
    // correctness once the doc exists, but a stable order keeps logs sane.
    let mut scope_order: Vec<String> = by_scope.keys().cloned().collect();
    scope_order.sort_by_key(|s| if s == "document" { 0 } else { 1 });

    let mut total_submitted = 0usize;
    for scope in &scope_order {
        let ops = by_scope.get(scope).unwrap();
        let mut actions: Vec<Value> = Vec::with_capacity(ops.len());
        for op in ops {
            actions.push(op_to_action(op)?);
        }
        let count = actions.len();
        submit_action_batch(client, doc_id, doc_name, scope, &actions).await?;
        progress.bump(count);
        total_submitted += count;
    }

    Ok(total_submitted)
}

/// Count the distinct `action.scope` values in an op list — that's how many
/// `mutateDocumentAsync` batches we'll need for the doc, since the reactor
/// rejects mixed-scope batches.
fn count_scopes(operations: &[Value]) -> usize {
    use std::collections::HashSet;
    let mut seen: HashSet<&str> = HashSet::new();
    for op in operations {
        let scope = op
            .pointer("/action/scope")
            .and_then(|v| v.as_str())
            .unwrap_or("global");
        seen.insert(scope);
    }
    seen.len()
}

/// Send one already-scope-homogeneous action batch via `mutateDocumentAsync`
/// and block on its job completion. Errors carry doc + scope context for
/// debuggability.
async fn submit_action_batch(
    client: &GraphQLClient,
    doc_id: &str,
    doc_name: &str,
    scope: &str,
    actions: &[Value],
) -> Result<()> {
    if actions.is_empty() {
        return Ok(());
    }
    let mutation = "mutation($di: String!, $acts: [JSONObject!]!) { mutateDocumentAsync(documentIdentifier: $di, actions: $acts) }";
    let vars = serde_json::json!({ "di": doc_id, "acts": actions });
    let resp = client.query(mutation, Some(&vars)).await.map_err(|e| {
        anyhow::anyhow!("submit failed for '{doc_name}' ({doc_id}) scope '{scope}': {e}")
    })?;
    let job_id = resp["mutateDocumentAsync"].as_str().ok_or_else(|| {
        anyhow::anyhow!(
            "mutateDocumentAsync returned no job id for '{doc_name}' ({doc_id}) scope '{scope}'"
        )
    })?;
    wait_for_job(client, job_id, REPLAY_JOB_TIMEOUT_MS)
        .await
        .map_err(|e| {
            anyhow::anyhow!("replay failed for '{doc_name}' ({doc_id}) scope '{scope}': {e}")
        })?;
    Ok(())
}

/// Convert a fetched `documentOperations` item into the action JSON shape
/// `mutateDocumentAsync` accepts: `{ id, type, input, scope, timestampUtcMs }`.
/// `context.signer` is already stripped by `fetch_document` (signatures are
/// bound to the source reactor's keys and would be rejected on the dest).
fn op_to_action(op: &Value) -> Result<Value> {
    let action = op
        .get("action")
        .ok_or_else(|| anyhow::anyhow!("operation missing 'action' field: {op}"))?;

    let action_id = action
        .get("id")
        .and_then(|v| v.as_str())
        .or_else(|| op.get("id").and_then(|v| v.as_str()))
        .map(String::from)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let op_type = action
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("operation missing 'action.type': {op}"))?
        .to_string();

    let scope = action
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("global")
        .to_string();

    let input = action
        .get("input")
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()));

    let timestamp = action
        .get("timestampUtcMs")
        .and_then(|v| v.as_str())
        .or_else(|| op.get("timestampUtcMs").and_then(|v| v.as_str()))
        .map(String::from)
        .unwrap_or_else(crate::cli::docs::iso_now);

    let mut act = serde_json::json!({
        "id": action_id,
        "type": op_type,
        "input": input,
        "scope": scope,
        "timestampUtcMs": timestamp,
    });

    // Pass through attachments and context (sans signer) if present — these
    // are part of the action's identity on the source and should land
    // unchanged on the destination.
    if let Some(att) = action.get("attachments")
        && !att.is_null()
    {
        act["attachments"] = att.clone();
    }
    if let Some(ctx) = action.get("context")
        && !ctx.is_null()
    {
        act["context"] = ctx.clone();
    }

    Ok(act)
}

/// Fetch just the display name and slug of a document. Used for the drive
/// itself because the broader metadata query in `fetch_document` requests
/// `createdAtUtcIso` / `lastModifiedAtUtcIso`, which are declared non-nullable
/// but sometimes come back null — taking the whole migration down.
async fn fetch_drive_label(client: &GraphQLClient, doc_id: &str) -> Result<(String, String)> {
    let escaped = doc_id.replace('"', r#"\""#);
    let query =
        format!(r#"{{ document(identifier: "{escaped}") {{ document {{ name slug state }} }} }}"#,);
    let data = client.query(&query, None).await?;
    let doc = data
        .pointer("/document/document")
        .filter(|v| !v.is_null())
        .ok_or_else(|| anyhow::anyhow!("Drive '{doc_id}' not found on source"))?;
    let name = doc
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let slug = doc
        .pointer("/state/global/slug")
        .and_then(|v| v.as_str())
        .or_else(|| doc.get("slug").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();
    Ok((name, slug))
}

/// Pull a document's full operation history with `context.signer` stripped
/// (signatures are tied to the source reactor's keys and would fail
/// verification on the destination). Skips the metadata round-trip that
/// `fetch_document` does — migrate doesn't need it, and avoiding it dodges
/// the schema-mismatch failure on `createdAtUtcIso`.
async fn fetch_operations(client: &GraphQLClient, doc_id: &str) -> Result<Vec<Value>> {
    let escaped = doc_id.replace('"', r#"\""#);
    let mut all_ops: Vec<Value> = Vec::new();
    let mut total_count: Option<usize> = None;
    loop {
        let offset = all_ops.len();
        let ops_query = format!(
            r#"{{ documentOperations(filter: {{ documentId: "{escaped}" }}, paging: {{ limit: {OP_BATCH_SIZE}, offset: {offset} }}) {{ items {{ id index action {{ id type input scope timestampUtcMs context {{ signer {{ user {{ address networkId chainId }} app {{ name key }} signatures }} }} }} timestampUtcMs hash skip error }} totalCount }} }}"#,
        );
        let ops_data = client.query(&ops_query, None).await?;
        if total_count.is_none() {
            total_count = ops_data
                .pointer("/documentOperations/totalCount")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
        }
        let batch = ops_data
            .pointer("/documentOperations/items")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let batch_len = batch.len();
        all_ops.extend(batch);
        if batch_len < OP_BATCH_SIZE {
            break;
        }
        if let Some(total) = total_count
            && all_ops.len() >= total
        {
            break;
        }
    }

    // Strip context.signer — same rationale as fetch_document in import_export.rs.
    for op in &mut all_ops {
        if let Some(action) = op.get_mut("action")
            && let Some(ctx) = action.get_mut("context")
            && let Some(obj) = ctx.as_object_mut()
        {
            obj.remove("signer");
        }
    }

    Ok(all_ops)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_to_action_extracts_action_fields() {
        let op = serde_json::json!({
            "id": "op-uuid",
            "index": 0,
            "timestampUtcMs": "1000",
            "action": {
                "id": "act-uuid",
                "type": "CREATE_DOCUMENT",
                "scope": "document",
                "input": { "id": "doc-uuid", "documentType": "powerhouse/document-drive" },
                "timestampUtcMs": "1000"
            }
        });
        let act = op_to_action(&op).unwrap();
        assert_eq!(act["id"], "act-uuid");
        assert_eq!(act["type"], "CREATE_DOCUMENT");
        assert_eq!(act["scope"], "document");
        assert_eq!(act["input"]["id"], "doc-uuid");
        assert_eq!(act["timestampUtcMs"], "1000");
    }

    #[test]
    fn op_to_action_defaults_scope_when_missing() {
        let op = serde_json::json!({
            "action": { "type": "SET_DRIVE_NAME", "input": { "name": "x" } }
        });
        let act = op_to_action(&op).unwrap();
        assert_eq!(act["scope"], "global");
    }

    #[test]
    fn op_to_action_errors_when_action_missing() {
        let op = serde_json::json!({ "id": "naked" });
        assert!(op_to_action(&op).is_err());
    }
}
