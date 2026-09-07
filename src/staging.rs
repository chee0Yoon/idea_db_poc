//! Durable storage for prepared MCP uploads.
//!
//! Staged uploads are operational packets, not domain `Record`s. They do not
//! consume or advance the domain sequence, do not move a head, and are absent
//! from authoritative exports. Neo4j persistence lets a client apply a packet
//! after a process restart. Applied packets are retained for exact replay
//! without another model call. A domain restore invalidates existing packets
//! in place, preserving their audit history while requiring a new preview.

use serde_json::{json, Value};

use crate::{
    error::{ApiError, ApiResult, Issue},
    limits, model,
    neo4j::{stmt, Neo4j},
    store, util,
};

const MAX_STAGING_ENTRIES: usize = 256;

fn validate_upload_id(upload_id: &str) -> ApiResult<()> {
    if upload_id.starts_with("upload_") && model::is_valid_id(upload_id) {
        Ok(())
    } else {
        Err(ApiError::bad_request(
            "upload_id must be a valid server-derived upload_* id",
        ))
    }
}

fn digest(value: Option<&Value>, field: &'static str) -> ApiResult<String> {
    let Some(text) = value.and_then(Value::as_str) else {
        return Err(ApiError::validation(vec![Issue::new(
            field,
            "required",
            format!("{field} is required"),
        )]));
    };
    let valid = text.len() == 71
        && text.starts_with("sha256:")
        && text[7..].bytes().all(|byte| byte.is_ascii_hexdigit());
    if !valid {
        return Err(ApiError::validation(vec![Issue::new(
            field,
            "invalid_digest",
            format!("{field} must be sha256:<64 hex>"),
        )]));
    }
    Ok(text.to_ascii_lowercase())
}

fn validate_document(document: &Value) -> ApiResult<(String, String, String)> {
    let object = document.as_object().ok_or_else(|| {
        ApiError::validation(vec![Issue::new(
            "document",
            "invalid_type",
            "staging document must be an object",
        )])
    })?;
    let raw_digest = digest(object.get("raw_request_digest"), "raw_request_digest")?;
    let prepared_digest = digest(object.get("prepared_digest"), "prepared_digest")?;
    let package = object.get("package").ok_or_else(|| {
        ApiError::validation(vec![Issue::new(
            "package",
            "required",
            "package is required",
        )])
    })?;
    if package.is_null() {
        return Err(ApiError::validation(vec![Issue::new(
            "package",
            "invalid_type",
            "package cannot be null",
        )]));
    }
    if prepared_digest != util::digest_json(package) {
        return Err(ApiError::digest_mismatch(
            &prepared_digest,
            &util::digest_json(package),
        ));
    }
    if !object.contains_key("summary") || object.get("summary").is_some_and(Value::is_null) {
        return Err(ApiError::validation(vec![Issue::new(
            "summary",
            "required",
            "summary is required",
        )]));
    }
    let created_at = object
        .get("created_at")
        .and_then(Value::as_str)
        .filter(|value| util::parse_rfc3339(value).is_some())
        .ok_or_else(|| {
            ApiError::validation(vec![Issue::new(
                "created_at",
                "invalid_timestamp",
                "created_at must be RFC 3339 with an offset",
            )])
        })?
        .to_string();
    let canonical = util::canonical_json(document);
    if canonical.len() > limits::MAX_BODY_BYTES {
        return Err(ApiError::validation(vec![Issue::new(
            "document",
            "limit_exceeded",
            format!(
                "staging packet is {} bytes; maximum is {}",
                canonical.len(),
                limits::MAX_BODY_BYTES
            ),
        )]));
    }
    Ok((raw_digest, created_at, canonical))
}

fn replay_key_and_size(document: &Value) -> ApiResult<(String, String)> {
    let object = document.as_object().ok_or_else(|| {
        ApiError::validation(vec![Issue::new(
            "document",
            "invalid_type",
            "staging document must be an object",
        )])
    })?;
    let raw_digest = digest(object.get("raw_request_digest"), "raw_request_digest")?;
    let canonical = util::canonical_json(document);
    if canonical.len() > limits::MAX_BODY_BYTES {
        return Err(ApiError::validation(vec![Issue::new(
            "document",
            "limit_exceeded",
            format!(
                "staging packet is {} bytes; maximum is {}",
                canonical.len(),
                limits::MAX_BODY_BYTES
            ),
        )]));
    }
    Ok((raw_digest, canonical))
}

fn decode_document(value: &Value, upload_id: &str) -> ApiResult<Value> {
    let raw = value.as_str().ok_or_else(|| {
        ApiError::storage(format!(
            "staged upload {upload_id} has a non-string document"
        ))
    })?;
    serde_json::from_str(raw).map_err(|error| {
        ApiError::storage(format!(
            "staged upload {upload_id} has unreadable JSON: {error}"
        ))
    })
}

fn invalidated(upload_id: &str) -> ApiError {
    ApiError::validation(vec![Issue::new(
        "upload_id",
        "staging_invalidated",
        format!(
            "staged upload {upload_id} was invalidated by a domain restore; prepare again with a new idempotency key"
        ),
    )])
}

fn withdrawn(upload_id: &str) -> ApiError {
    ApiError::validation(vec![Issue::new(
        "upload_id",
        "staging_withdrawn",
        format!("staged upload {upload_id} was withdrawn; prepare a new upload"),
    )])
}

fn capacity_error() -> ApiError {
    ApiError::validation(vec![Issue::new(
        "staging",
        "capacity_exceeded",
        format!(
            "pending staging capacity is {MAX_STAGING_ENTRIES}; no history was automatically evicted"
        ),
    )])
}

/// Cheap advisory capacity check for callers to run before expensive local
/// model work. `put` repeats it under the database lock because this read-only
/// check can race with another client.
pub async fn check_capacity(neo: &Neo4j) -> ApiResult<Value> {
    let results = neo
        .run(&[stmt(
            "MATCH (s:IdeaDbUpload) \
             WHERE s.applied_seq IS NULL AND s.invalidated_at IS NULL AND s.withdrawn_at IS NULL \
             RETURN count(s) AS pending",
            json!({}),
        )])
        .await?;
    let pending = results
        .first()
        .and_then(|result| result.col(0, "pending"))
        .and_then(Value::as_i64)
        .ok_or_else(|| ApiError::storage("could not count pending staged uploads"))?;
    if pending >= MAX_STAGING_ENTRIES as i64 {
        return Err(capacity_error());
    }
    Ok(json!({
        "pending": pending,
        "capacity": MAX_STAGING_ENTRIES,
        "remaining": MAX_STAGING_ENTRIES as i64 - pending,
    }))
}

/// Fetch a prepared upload without taking the domain write lock. One Cypher
/// statement is an atomic read of one immutable staging node.
pub async fn get(neo: &Neo4j, upload_id: &str) -> ApiResult<Option<Value>> {
    validate_upload_id(upload_id)?;
    let results = neo
        .run(&[stmt(
            "MATCH (s:IdeaDbUpload {upload_id: $upload_id}) \
             RETURN s.document AS document, s.invalidated_at AS invalidated_at, \
             s.withdrawn_at AS withdrawn_at LIMIT 1",
            json!({"upload_id": upload_id}),
        )])
        .await?;
    let Some(result) = results.first() else {
        return Ok(None);
    };
    if !result.col(0, "invalidated_at").is_none_or(Value::is_null) {
        return Err(invalidated(upload_id));
    }
    if !result.col(0, "withdrawn_at").is_none_or(Value::is_null) {
        return Err(withdrawn(upload_id));
    }
    result
        .col(0, "document")
        .map(|value| decode_document(value, upload_id))
        .transpose()
}

/// Insert an immutable prepared upload. The global database lock serialises
/// capacity checks and the check-before-create idempotency decision across API
/// processes while leaving the domain sequence untouched.
pub async fn put(neo: &Neo4j, upload_id: &str, document: Value) -> ApiResult<Value> {
    validate_upload_id(upload_id)?;
    // The replay key is sufficient to return an already prepared immutable
    // packet. Other fields are validated only when a new node will be created.
    let (raw_digest, _) = replay_key_and_size(&document)?;
    let mut tx = neo.begin().await?;
    let result = async {
        store::lock(&mut tx).await?;
        let existing = tx
            .run_one(stmt(
                "MATCH (s:IdeaDbUpload {upload_id: $upload_id}) \
                 RETURN s.raw_request_digest AS raw_request_digest, s.document AS document, \
                 s.invalidated_at AS invalidated_at, s.withdrawn_at AS withdrawn_at LIMIT 1",
                json!({"upload_id": upload_id}),
            ))
            .await?;
        if !existing.rows.is_empty() {
            if !existing.col(0, "invalidated_at").is_none_or(Value::is_null) {
                return Err(invalidated(upload_id));
            }
            if !existing.col(0, "withdrawn_at").is_none_or(Value::is_null) {
                return Err(withdrawn(upload_id));
            }
            let stored_digest = existing
                .col(0, "raw_request_digest")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    ApiError::storage(format!(
                        "staged upload {upload_id} has no raw request digest"
                    ))
                })?;
            if stored_digest != raw_digest {
                return Err(ApiError::idempotency_conflict(upload_id));
            }
            let stored = existing.col(0, "document").ok_or_else(|| {
                ApiError::storage(format!("staged upload {upload_id} has no document"))
            })?;
            return Ok((decode_document(stored, upload_id)?, false));
        }
        let (_, created_at, canonical) = validate_document(&document)?;
        let count = tx
            .run_one(stmt(
                "MATCH (s:IdeaDbUpload) \
                 WHERE s.applied_seq IS NULL AND s.invalidated_at IS NULL AND s.withdrawn_at IS NULL \
                 RETURN count(s) AS count",
                json!({}),
            ))
            .await?
            .col(0, "count")
            .and_then(Value::as_i64)
            .ok_or_else(|| ApiError::storage("could not count staged uploads"))?;
        if count >= MAX_STAGING_ENTRIES as i64 {
            return Err(capacity_error());
        }
        tx.run_one(stmt(
            "CREATE (s:IdeaDbUpload {upload_id: $upload_id, raw_request_digest: $raw_request_digest, \
             prepared_digest: $prepared_digest, created_at: $created_at, document: $document}) \
             RETURN s.upload_id AS upload_id",
            json!({
                "upload_id": upload_id,
                "raw_request_digest": raw_digest,
                "prepared_digest": document.get("prepared_digest"),
                "created_at": created_at,
                "document": canonical,
            }),
        ))
        .await?;
        Ok((document, true))
    }
    .await;
    match result {
        Ok((packet, true)) => {
            tx.commit(&[]).await?;
            Ok(packet)
        }
        Ok((packet, false)) => {
            tx.rollback().await;
            Ok(packet)
        }
        Err(error) => {
            tx.rollback().await;
            Err(error)
        }
    }
}

/// Retain an abandoned prepared upload while releasing its pending-capacity slot.
pub async fn discard(neo: &Neo4j, upload_id: &str) -> ApiResult<Value> {
    validate_upload_id(upload_id)?;
    let mut tx = neo.begin().await?;
    let outcome = async {
        store::lock(&mut tx).await?;
        let existing = tx
            .run_one(stmt(
                "MATCH (s:IdeaDbUpload {upload_id:$upload_id}) \
             RETURN s.applied_seq AS applied_seq, s.invalidated_at AS invalidated_at, \
             s.withdrawn_at AS withdrawn_at LIMIT 1",
                json!({"upload_id":upload_id}),
            ))
            .await?;
        if existing.rows.is_empty() {
            return Err(ApiError::not_found(format!(
                "staged upload {upload_id} not found"
            )));
        }
        if !existing.col(0, "applied_seq").is_none_or(Value::is_null) {
            return Err(ApiError {
                status: axum::http::StatusCode::CONFLICT,
                code: "staging_already_applied",
                message: format!(
                    "staged upload {upload_id} was already applied and cannot be withdrawn"
                ),
                details: None,
            });
        }
        if !existing.col(0, "invalidated_at").is_none_or(Value::is_null) {
            return Err(invalidated(upload_id));
        }
        if let Some(at) = existing.col(0, "withdrawn_at").and_then(Value::as_str) {
            return Ok((
                json!({"upload_id":upload_id,"withdrawn_at":at,"discarded":true,"replay":true}),
                false,
            ));
        }
        let at = util::now_utc_millis();
        let updated = tx
            .run_one(stmt(
                "MATCH (s:IdeaDbUpload {upload_id:$upload_id}) \
             WHERE s.applied_seq IS NULL AND s.invalidated_at IS NULL AND s.withdrawn_at IS NULL \
             SET s.withdrawn_at=$withdrawn_at RETURN count(s) AS withdrawn",
                json!({"upload_id":upload_id,"withdrawn_at":at}),
            ))
            .await?;
        if updated.col(0, "withdrawn").and_then(Value::as_i64) != Some(1) {
            return Err(ApiError::storage(format!(
                "staged upload {upload_id} could not be withdrawn"
            )));
        }
        Ok((
            json!({"upload_id":upload_id,"withdrawn_at":at,"discarded":true,"replay":false}),
            true,
        ))
    }
    .await;
    match outcome {
        Ok((value, true)) => {
            tx.commit(&[]).await?;
            Ok(value)
        }
        Ok((value, false)) => {
            tx.rollback().await;
            Ok(value)
        }
        Err(error) => {
            tx.rollback().await;
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet() -> Value {
        let package = json!({"protocol_version":1,"records":[]});
        json!({
            "raw_request_digest": util::digest_json(&json!({"source":"raw"})),
            "prepared_digest": util::digest_json(&package),
            "package": package,
            "summary": {"records":0,"embeddings":0},
            "created_at": "2026-09-07T00:00:00Z"
        })
    }

    #[test]
    fn validates_packet_digest_and_id() {
        validate_upload_id("upload_0123456789abcdef").unwrap();
        assert!(validate_upload_id("upload bad").is_err());
        validate_document(&packet()).unwrap();
        let mut changed = packet();
        changed["package"]["records"] = json!([{"id":"changed"}]);
        assert_eq!(
            validate_document(&changed).unwrap_err().code,
            "digest_mismatch"
        );
    }

    #[test]
    fn rejects_packets_over_two_mebibytes() {
        let mut value = packet();
        value["summary"] = json!({"padding":"x".repeat(limits::MAX_BODY_BYTES)});
        let error = validate_document(&value).unwrap_err();
        assert_eq!(error.code, "validation_failed");
        assert_eq!(
            error.details.unwrap()["issues"][0]["code"],
            "limit_exceeded"
        );
    }
}
