//! Minimal client for the Neo4j HTTP explicit-transaction API.
//!
//! `POST /db/{db}/tx` opens a transaction, `POST /db/{db}/tx/{id}` adds
//! statements, `POST .../commit` commits and `DELETE /db/{db}/tx/{id}` rolls
//! back. These endpoints are deprecated in 5.26 but present and stable; the MVP
//! is pinned to 5.26.30 Community. Moving past that pin means switching to the
//! Query API v2. `docs/api.md` §1.

use std::time::Duration;

use serde_json::{json, Value};

use crate::config::Config;
use crate::error::{ApiError, ApiResult};

#[derive(Debug, Clone)]
pub struct Statement {
    pub statement: String,
    pub parameters: Value,
}

pub fn stmt(statement: impl Into<String>, parameters: Value) -> Statement {
    Statement {
        statement: statement.into(),
        parameters,
    }
}

#[derive(Debug, Clone, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

impl QueryResult {
    pub fn col(&self, row: usize, name: &str) -> Option<&Value> {
        let ix = self.columns.iter().position(|c| c == name)?;
        self.rows.get(row)?.get(ix)
    }

    pub fn column<'a>(&'a self, name: &str) -> impl Iterator<Item = &'a Value> + 'a {
        let ix = self.columns.iter().position(|c| c == name);
        self.rows
            .iter()
            .filter_map(move |r| ix.and_then(|i| r.get(i)))
    }
}

#[derive(Debug, Clone)]
pub struct Neo4j {
    client: reqwest::Client,
    base: String,
    database: String,
    user: String,
    password: Option<String>,
}

impl Neo4j {
    pub fn new(cfg: &Config) -> ApiResult<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| ApiError::storage(format!("http client: {e}")))?;
        Ok(Neo4j {
            client,
            base: cfg.neo4j_uri.clone(),
            database: cfg.neo4j_database.clone(),
            user: cfg.neo4j_user.clone(),
            password: cfg.neo4j_password.clone(),
        })
    }

    pub fn uri(&self) -> &str {
        &self.base
    }

    pub fn database(&self) -> &str {
        &self.database
    }

    fn request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        let rb = self.client.request(method, url);
        match &self.password {
            Some(pw) => rb.basic_auth(&self.user, Some(pw)),
            None => rb,
        }
    }

    async fn post(
        &self,
        url: &str,
        statements: &[Statement],
    ) -> ApiResult<(Value, Vec<QueryResult>)> {
        let body = json!({
            "statements": statements.iter().map(|s| json!({
                "statement": s.statement,
                "parameters": s.parameters,
            })).collect::<Vec<_>>()
        });
        let resp = self
            .request(reqwest::Method::POST, url)
            .json(&body)
            .send()
            .await
            .map_err(|e| ApiError::storage(format!("neo4j request failed: {e}")))?;
        let status = resp.status();
        let payload: Value = resp.json().await.map_err(|e| {
            ApiError::storage(format!("neo4j returned a non-JSON body ({status}): {e}"))
        })?;
        if let Some(err) = first_error(&payload) {
            return Err(ApiError::storage(err));
        }
        if !status.is_success() {
            return Err(ApiError::storage(format!("neo4j http {status}")));
        }
        Ok((payload.clone(), parse_results(&payload)))
    }

    /// One-shot transaction, used for bootstrap and read-only queries.
    pub async fn run(&self, statements: &[Statement]) -> ApiResult<Vec<QueryResult>> {
        let url = format!("{}/db/{}/tx/commit", self.base, self.database);
        Ok(self.post(&url, statements).await?.1)
    }

    /// Open an explicit transaction that stays open across Rust validation.
    pub async fn begin(&self) -> ApiResult<Tx> {
        let url = format!("{}/db/{}/tx", self.base, self.database);
        let (payload, _) = self.post(&url, &[]).await?;
        let commit_url = payload
            .get("commit")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::storage("neo4j did not return a commit url"))?
            .to_string();
        let tx_url = commit_url
            .strip_suffix("/commit")
            .unwrap_or(&commit_url)
            .to_string();
        Ok(Tx {
            neo: self.clone(),
            commit_url,
            tx_url,
            open: true,
        })
    }
}

fn first_error(payload: &Value) -> Option<String> {
    let errors = payload.get("errors")?.as_array()?;
    let e = errors.first()?;
    Some(format!(
        "{}: {}",
        e.get("code").and_then(Value::as_str).unwrap_or("Neo.Error"),
        e.get("message").and_then(Value::as_str).unwrap_or("")
    ))
}

fn parse_results(payload: &Value) -> Vec<QueryResult> {
    payload
        .get("results")
        .and_then(Value::as_array)
        .map(|results| {
            results
                .iter()
                .map(|r| QueryResult {
                    columns: r
                        .get("columns")
                        .and_then(Value::as_array)
                        .map(|c| {
                            c.iter()
                                .map(|v| v.as_str().unwrap_or_default().to_string())
                                .collect()
                        })
                        .unwrap_or_default(),
                    rows: r
                        .get("data")
                        .and_then(Value::as_array)
                        .map(|d| {
                            d.iter()
                                .map(|row| {
                                    row.get("row")
                                        .and_then(Value::as_array)
                                        .cloned()
                                        .unwrap_or_default()
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// An open Neo4j transaction. Always finished explicitly with `commit` or
/// `rollback`; a leaked transaction is released by the server's own timeout.
pub struct Tx {
    neo: Neo4j,
    commit_url: String,
    tx_url: String,
    open: bool,
}

impl Tx {
    pub async fn run(&mut self, statements: &[Statement]) -> ApiResult<Vec<QueryResult>> {
        Ok(self.neo.post(&self.tx_url.clone(), statements).await?.1)
    }

    pub async fn run_one(&mut self, statement: Statement) -> ApiResult<QueryResult> {
        let mut out = self.run(&[statement]).await?;
        Ok(if out.is_empty() {
            QueryResult::default()
        } else {
            out.remove(0)
        })
    }

    pub async fn commit(mut self, statements: &[Statement]) -> ApiResult<Vec<QueryResult>> {
        self.open = false;
        Ok(self.neo.post(&self.commit_url.clone(), statements).await?.1)
    }

    /// Best-effort rollback: a failure here still leaves nothing committed.
    pub async fn rollback(mut self) {
        if !self.open {
            return;
        }
        self.open = false;
        let _ = self
            .neo
            .request(reqwest::Method::DELETE, &self.tx_url)
            .send()
            .await;
    }
}
