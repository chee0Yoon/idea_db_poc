//! MCP intake: the client supplies structure; this module indexes it, never atomizes it.
use std::time::Duration;

use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    error::{ApiError, ApiResult, Issue},
    limits,
    model::EntityKind,
    mutation,
    neo4j::Neo4j,
    query, store, util,
    validate::{Context, Package},
};

const FORMAT: &str = "idea-body-v1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Preview {
    #[serde(default)]
    package: Option<Value>,
    #[serde(default)]
    replacement: Option<Value>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepared {
    upload_id: String,
    prepared_digest: String,
}

struct Provider {
    client: reqwest::Client,
    url: String,
    model: String,
    profile: String,
}

fn embedding_error(code: &'static str, message: &str) -> ApiError {
    ApiError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        code,
        message: message.into(),
        details: None,
    }
}

impl Provider {
    async fn configured() -> ApiResult<Self> {
        let url = std::env::var("IDEA_DB_EMBEDDING_URL").map_err(|_| embedding_error("embedding_not_configured", "Configure the local Ollama IDEA_DB_EMBEDDING_URL and IDEA_DB_EMBEDDING_MODEL before upload."))?;
        let model = std::env::var("IDEA_DB_EMBEDDING_MODEL").map_err(|_| {
            embedding_error(
                "embedding_not_configured",
                "IDEA_DB_EMBEDDING_MODEL is required.",
            )
        })?;
        let parsed = reqwest::Url::parse(&url)
            .map_err(|_| embedding_error("embedding_not_configured", "Invalid embedding URL."))?;
        // This is a deployment setting, never a tool argument. Disallow credentials,
        // redirection and nonlocal hosts so ordinary uploads cannot export source text.
        if !matches!(parsed.scheme(), "http" | "https")
            || !matches!(
                parsed.host_str(),
                Some("127.0.0.1" | "localhost" | "::1" | "[::1]" | "host.docker.internal")
            )
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || parsed.path() != "/"
        {
            return Err(embedding_error("embedding_not_local", "Embedding URL must be a local Ollama origin without credentials, path, query or fragment."));
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(90))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|_| {
                embedding_error("embedding_unavailable", "Cannot create embedding client.")
            })?;
        let base = url.trim_end_matches('/').to_string();
        let tags = bounded_json(client.get(format!("{base}/api/tags")).send().await).await?;
        let name = if model.contains(':') {
            model.clone()
        } else {
            format!("{model}:latest")
        };
        let digest = tags["models"]
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .find(|m| m["name"] == name || m["model"] == name)
            })
            .and_then(|m| m["digest"].as_str())
            .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| {
                embedding_error(
                    "embedding_model_missing",
                    "Configured local embedding model is not installed or has no immutable digest.",
                )
            })?;
        let profile = format!("{name}@sha256:{digest}/{FORMAT}");
        if profile.len() > 200 {
            return Err(embedding_error(
                "embedding_not_configured",
                "Embedding model identity is too long.",
            ));
        }
        Ok(Self {
            client,
            url: base,
            model: name,
            profile,
        })
    }

    async fn embed(&self, text: &str) -> ApiResult<Vec<f64>> {
        let value = bounded_json(
            self.client
                .post(format!("{}/api/embed", self.url))
                .json(&json!({"model":self.model,"input":text,"truncate":false}))
                .send()
                .await,
        )
        .await?;
        let rows = value["embeddings"]
            .as_array()
            .filter(|x| x.len() == 1)
            .ok_or_else(|| {
                embedding_error(
                    "embedding_invalid_response",
                    "Expected one embedding per input.",
                )
            })?;
        let vector: Vec<f64> = serde_json::from_value(rows[0].clone()).map_err(|_| {
            embedding_error(
                "embedding_invalid_response",
                "Embedding values must be finite numbers.",
            )
        })?;
        check_vector(&vector)?;
        Ok(vector)
    }

    async fn verify_unchanged(&self) -> ApiResult<()> {
        if Self::configured().await?.profile != self.profile {
            return Err(embedding_error(
                "embedding_model_changed",
                "Local model changed while embedding; retry preview with a stable model.",
            ));
        }
        Ok(())
    }
}

async fn bounded_json(response: Result<reqwest::Response, reqwest::Error>) -> ApiResult<Value> {
    let mut response = response.map_err(|_| {
        embedding_error(
            "embedding_unavailable",
            "Local embedding service did not answer.",
        )
    })?;
    if !response.status().is_success() {
        return Err(embedding_error(
            "embedding_provider_error",
            "Local embedding failed (input may exceed the model context); nothing was committed.",
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| embedding_error("embedding_unavailable", "Incomplete embedding response."))?
    {
        if bytes.len() + chunk.len() > limits::MAX_BODY_BYTES {
            return Err(embedding_error(
                "embedding_invalid_response",
                "Embedding response exceeded its bound.",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| {
        embedding_error(
            "embedding_invalid_response",
            "Embedding service returned invalid JSON.",
        )
    })
}

fn check_vector(vector: &[f64]) -> ApiResult<()> {
    let norm = vector.iter().map(|x| x * x).sum::<f64>();
    if vector.is_empty()
        || vector.len() > limits::MAX_EMBEDDING_DIM
        || !norm.is_finite()
        || norm <= 0.0
        || vector.iter().any(|x| !x.is_finite())
    {
        return Err(embedding_error(
            "embedding_invalid_response",
            "Embedding must be a finite nonzero vector with a supported dimension.",
        ));
    }
    Ok(())
}

fn embedding_id(revision_id: &str, body: &str, profile: &str) -> String {
    util::derived_id(
        "emb",
        &util::canonical_json(&json!([revision_id, util::digest_text(body), profile])),
    )
}

fn parse_package(body: Value) -> ApiResult<Package> {
    serde_json::from_value(body).map_err(|e| ApiError::bad_request(e.to_string()))
}

fn ideas<'a>(pkg: &'a Package, ctx: &Context) -> Vec<(&'a str, &'a str, String)> {
    pkg.records
        .iter()
        .filter_map(|r| {
            let revision: crate::model::RevisionData =
                if r.kind == crate::model::RecordKind::Revision {
                    serde_json::from_value(r.data.clone()).ok()?
                } else {
                    return None;
                };
            let entity = pkg
                .records
                .iter()
                .find(|x| x.id == revision.entity_id)
                .and_then(|x| {
                    serde_json::from_value::<crate::model::EntityData>(x.data.clone()).ok()
                })
                .or_else(|| {
                    ctx.records
                        .get(&revision.entity_id)
                        .and_then(|x| x.as_entity())
                        .cloned()
                })?;
            if entity.entity_kind != EntityKind::Idea {
                return None;
            }
            Some((r.id.as_str(), r.data["body"].as_str()?, entity.project_id))
        })
        .collect()
}

/// Embeddings and suggestions are review material. Only apply can commit them.
pub async fn preview(neo: &Neo4j, body: Value) -> ApiResult<Value> {
    if util::canonical_json(&body).len() > limits::MAX_BODY_BYTES {
        return Err(ApiError::bad_request("Upload input exceeds 2 MiB."));
    }
    let raw_request_digest = util::digest_json(&body);
    let request: Preview =
        serde_json::from_value(body).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let upload_id = util::derived_id("upload", &raw_request_digest);
    if let Some(staged) = crate::staging::get(neo, &upload_id).await? {
        if staged["raw_request_digest"] != raw_request_digest {
            return Err(ApiError::idempotency_conflict(&upload_id));
        }
        return Ok(staged["summary"].clone());
    }
    // Match domain idempotency precedence before duplicate record validation or
    // embedding work. An exact retry already returned its durable stage above.
    let requested_key = request
        .package
        .as_ref()
        .or(request.replacement.as_ref())
        .and_then(|value| value.get("idempotency_key"))
        .and_then(Value::as_str);
    if let Some(key) = requested_key {
        let mut tx = neo.begin().await?;
        let receipt = store::find_receipt(&mut tx, key).await;
        tx.rollback().await;
        if receipt?.is_some() {
            return Err(ApiError::idempotency_conflict(key));
        }
    }
    crate::staging::check_capacity(neo).await?;
    let (mut package, replacement_result) = match (request.package, request.replacement) {
        (Some(package), None) => (package, Value::Null),
        (None, Some(replacement)) => {
            let prepared = mutation::preview_replacement(neo, replacement).await?;
            (
                prepared["package"].clone(),
                prepared["replacement_result"].clone(),
            )
        }
        _ => {
            return Err(ApiError::bad_request(
                "Provide exactly one of package or replacement.",
            ))
        }
    };
    let pkg = parse_package(package.clone())?;
    mutation::check_not_noop(&pkg)?;
    if pkg
        .records
        .iter()
        .any(|r| r.kind == crate::model::RecordKind::Embedding)
    {
        return Err(ApiError::validation(vec![Issue::new(
            "records",
            "client_embeddings_forbidden",
            "Embedding records are generated by MCP intake, not accepted from client packages.",
        )]));
    }
    // The public validator includes current expected-head/CAS and capacity checks.
    // Reject its `valid:false` projection before any provider work or staging write.
    require_valid(mutation::validate_only(neo, package.clone()).await?)?;
    let ctx = store::read_context(neo).await?;
    let atoms = ideas(&pkg, &ctx);
    let required = atoms.len();
    if pkg.records.len() + required > limits::MAX_PACKAGE_RECORDS
        || ctx.records.len() + pkg.records.len() + required + 2 > limits::MAX_PREFETCH_RECORDS
    {
        return Err(ApiError::validation(vec![Issue::new("records", "limit_exceeded", "Preview would exceed package/namespace capacity after indexing; submit a smaller reviewed package.")]));
    }
    let mut mappings = Vec::new();
    let mut profile = Value::Null;
    if !atoms.is_empty() {
        let provider = Provider::configured().await?;
        profile = json!(provider.profile);
        let mut dimension = None;
        for (revision_id, text, project_id) in atoms {
            let vector = provider.embed(text).await?;
            if dimension.is_some_and(|d| d != vector.len()) {
                return Err(embedding_error(
                    "embedding_dimension_changed",
                    "Embedding dimensions changed within the batch.",
                ));
            }
            if dimension.is_none()
                && util::canonical_json(&package).len() * 2
                    + required * (vector.len() * 26 + 16_384)
                    > limits::MAX_BODY_BYTES
            {
                return Err(ApiError::validation(vec![Issue::new(
                    "records",
                    "limit_exceeded",
                    "Estimated embedding/staging size exceeds 2 MiB; submit fewer Ideas.",
                )]));
            }
            dimension = Some(vector.len());
            let id = embedding_id(revision_id, text, &provider.profile);
            let records = package["records"]
                .as_array_mut()
                .ok_or_else(|| ApiError::bad_request("records must be an array"))?;
            // A new preview recomputes embeddings for new Ideas, never trusts stale vectors.
            records
                .retain(|x| !(x["kind"] == "embedding" && x["data"]["revision_id"] == revision_id));
            records.push(json!({"id":id,"kind":"embedding","data":{"revision_id":revision_id,"model":provider.profile,"dim":vector.len(),"values":vector,"normalized":false}}));
            if ctx.records.contains_key(&project_id) {
                let hits = query::search(
                    &ctx,
                    json!({"project_id":project_id,"scope":"project_history","query":text,"vector":{"model":provider.profile,"dim":vector.len(),"values":vector},"limit":10}),
                )?;
                mappings.push(json!({"revision_id":revision_id,"suggestions":hits["results"],"related":hits["related"],"vector_status":hits["vector_status"],"requires_semantic_review":true}));
            } else {
                mappings.push(json!({"revision_id":revision_id,"suggestions":[],"requires_semantic_review":true}));
            }
        }
        provider.verify_unchanged().await?;
    }
    // Embeddings changed the package and the head may have moved while the local
    // provider ran. Revalidate the exact packet immediately before staging it.
    let validation = require_valid(mutation::validate_only(neo, package.clone()).await?)?;
    let prepared_digest = util::digest_json(&package);
    let summary = json!({"upload_id":upload_id,"prepared_digest":prepared_digest,"validation":validation,"embedding_profile":profile,"mappings":mappings,"semantic_atomicity":"client_review_required","automatically_merged":false,"replacement_result":replacement_result,"records":package["records"].as_array().map(Vec::len),"review_records":package["records"].as_array().map(|records|records.iter().filter(|r|r["kind"]!="embedding").cloned().collect::<Vec<_>>())});
    let stored = crate::staging::put(neo,&upload_id,json!({"raw_request_digest":raw_request_digest,"prepared_digest":prepared_digest,"package":package,"summary":summary,"created_at":util::now_utc_millis()})).await?;
    Ok(stored["summary"].clone())
}

fn require_valid(validation: Value) -> ApiResult<Value> {
    if validation["valid"] == true {
        return Ok(validation);
    }
    let errors = validation["errors"].clone();
    if errors
        .as_array()
        .is_some_and(|items| items.iter().any(|issue| issue["code"] == "head_conflict"))
    {
        return Err(ApiError::head_conflict(json!({"errors": errors})));
    }
    let message = errors
        .as_array()
        .and_then(|items| items.first())
        .and_then(|issue| issue["message"].as_str())
        .unwrap_or("package validation failed")
        .to_string();
    Err(ApiError {
        status: StatusCode::UNPROCESSABLE_ENTITY,
        code: "validation_failed",
        message,
        details: Some(json!({"issues": errors})),
    })
}

/// Apply is offline with respect to the model: exact packet retries remain stable.
pub async fn apply(neo: &Neo4j, body: Value) -> ApiResult<Value> {
    let request: Prepared =
        serde_json::from_value(body).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let stored = crate::staging::get(neo, &request.upload_id)
        .await?
        .ok_or_else(|| {
            ApiError::not_found(
                "Prepared upload does not exist; preview it on this database first.",
            )
        })?;
    let package = stored["package"].clone();
    if util::digest_json(&package) != request.prepared_digest
        || stored["prepared_digest"] != request.prepared_digest
    {
        return Err(ApiError::bad_request(
            "prepared_digest mismatch; apply only the reviewed upload handle",
        ));
    }
    let pkg = parse_package(package.clone())?;
    let ctx = store::read_context(neo).await?;
    for (id, text, _) in ideas(&pkg, &ctx) {
        let embeddings: Vec<_> = pkg
            .records
            .iter()
            .filter(|r| {
                r.kind == crate::model::RecordKind::Embedding && r.data["revision_id"] == id
            })
            .collect();
        if embeddings.len() != 1 {
            return Err(ApiError::validation(vec![Issue::new(
                "records",
                "embedding_required",
                "Each uploaded Idea revision needs exactly one prepared embedding.",
            )]));
        }
        let e: crate::model::EmbeddingData = serde_json::from_value(embeddings[0].data.clone())
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
        if !e.model.ends_with(&format!("/{FORMAT}"))
            || !e.model.contains("@sha256:")
            || embeddings[0].id != embedding_id(id, text, &e.model)
            || e.dim != e.values.len()
        {
            return Err(ApiError::bad_request("Embedding identity does not bind this revision, body and model profile; preview again."));
        }
        check_vector(&e.values)?;
    }
    let mut result = mutation::apply_staged(neo, package, &request.upload_id).await?;
    if let Some(metadata) = stored["summary"]["replacement_result"].as_object() {
        for (key, value) in metadata {
            result[key] = value.clone();
        }
    }
    Ok(result)
}

/// Compatibility convenience for an explicitly requested occurrence edit. The
/// graph operation remains atomic and all new Ideas pass the same indexing gate.
pub async fn replace(neo: &Neo4j, body: Value) -> ApiResult<Value> {
    let prepared = preview(neo, json!({"replacement":body})).await?;
    apply(
        neo,
        json!({"upload_id":prepared["upload_id"],"prepared_digest":prepared["prepared_digest"]}),
    )
    .await
}

/// Explicit lexical mode remains useful without a model; hybrid never silently degrades.
pub async fn search(neo: &Neo4j, mut body: Value) -> ApiResult<Value> {
    let mode = body
        .as_object_mut()
        .ok_or_else(|| ApiError::bad_request("search must be an object"))?
        .remove("embedding_mode")
        .unwrap_or(json!("hybrid"));
    if mode != "hybrid" && mode != "lexical" {
        return Err(ApiError::bad_request(
            "embedding_mode must be hybrid or lexical",
        ));
    }
    if body.get("vector").is_some_and(|v| !v.is_null()) {
        return Err(ApiError::bad_request(
            "MCP search generates its query vector; use embedding_mode lexical to disable it.",
        ));
    }
    let mut profile = Value::Null;
    if mode == "hybrid" {
        let text = body["query"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| ApiError::bad_request("Hybrid search needs a nonempty query."))?;
        let provider = Provider::configured().await?;
        let vector = provider.embed(text).await?;
        provider.verify_unchanged().await?;
        profile = json!(provider.profile);
        body["vector"] = json!({"model":provider.profile,"dim":vector.len(),"values":vector});
    }
    let ctx = store::read_context(neo).await?;
    let mut found = query::search(&ctx, body)?;
    found["index_coverage"] = index_coverage(&mode, &found["vector_status"]);
    found["embedding_mode"] = mode;
    found["embedding_profile"] = profile;
    Ok(found)
}

fn index_coverage(mode: &Value, vector_status: &Value) -> Value {
    let eligible = vector_status["eligible_idea_revisions"]
        .as_u64()
        .unwrap_or(0);
    let indexed = vector_status["ideas_with_matching_embedding"]
        .as_u64()
        .unwrap_or(0);
    if mode == "hybrid" {
        json!({"checked":true,"eligible_idea_revisions":eligible,"indexed_idea_revisions":indexed,"unindexed_idea_revisions":eligible.saturating_sub(indexed),"complete":indexed==eligible,"note":"Coverage counts only eligible Idea revisions for the requested immutable model profile."})
    } else {
        json!({"checked":false,"eligible_idea_revisions":eligible,"indexed_idea_revisions":Value::Null,"unindexed_idea_revisions":Value::Null,"complete":Value::Null,"note":"Lexical search did not request an embedding profile, so index completeness was not evaluated."})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vector_validation_and_content_binding() {
        assert!(check_vector(&[]).is_err());
        assert!(check_vector(&[0.0, 0.0]).is_err());
        assert!(check_vector(&[f64::INFINITY]).is_err());
        assert!(check_vector(&[f64::MAX]).is_err());
        assert!(check_vector(&[0.2, -0.5]).is_ok());
        assert_ne!(
            embedding_id("r", "30초", "m1"),
            embedding_id("r", "20초", "m1")
        );
        assert_ne!(
            embedding_id("r", "30초", "m1"),
            embedding_id("r", "30초", "m2")
        );
    }

    #[test]
    fn invalid_validation_projection_is_never_accepted_for_staging() {
        assert!(require_valid(json!({"valid":true,"errors":[]})).is_ok());
        let invalid = require_valid(json!({
            "valid":false,
            "errors":[{"path":"records","code":"empty_commit","message":"empty"}]
        }))
        .unwrap_err();
        assert_eq!(invalid.code, "validation_failed");
        assert_eq!(invalid.status, StatusCode::UNPROCESSABLE_ENTITY);

        let stale = require_valid(json!({
            "valid":false,
            "errors":[{"path":"expected_heads","code":"head_conflict","message":"stale"}]
        }))
        .unwrap_err();
        assert_eq!(stale.code, "head_conflict");
        assert_eq!(stale.status, StatusCode::CONFLICT);
    }

    #[test]
    fn index_coverage_counts_only_ideas_and_lexical_is_unchecked() {
        let status = json!({
            "eligible_revisions": 9,
            "with_matching_embedding": 7,
            "eligible_idea_revisions": 3,
            "ideas_with_matching_embedding": 2
        });
        let hybrid = index_coverage(&json!("hybrid"), &status);
        assert_eq!(hybrid["eligible_idea_revisions"], 3);
        assert_eq!(hybrid["indexed_idea_revisions"], 2);
        assert_eq!(hybrid["complete"], false);
        let lexical = index_coverage(&json!("lexical"), &status);
        assert_eq!(lexical["checked"], false);
        assert!(lexical["complete"].is_null());
        assert!(lexical["indexed_idea_revisions"].is_null());
    }
}
