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
        // Keep the HTTP exchange alive if the caller is cancelled. If Neo4j
        // created a transaction, the completed task owns a `Tx`; dropping its
        // unobserved output then triggers the same cleanup as any other owner.
        let neo = self.clone();
        tokio::spawn(async move { neo.begin_inner().await })
            .await
            .map_err(|error| ApiError::storage(format!("neo4j begin task failed: {error}")))?
    }

    async fn begin_inner(&self) -> ApiResult<Tx> {
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
            state: TxState::Open,
        })
    }

    async fn delete_transaction(&self, tx_url: &str) -> bool {
        const BACKOFF_MS: &[u64] = &[20, 50, 100, 200];
        for attempt in 0..=BACKOFF_MS.len() {
            let response = match self.request(reqwest::Method::DELETE, tx_url).send().await {
                Ok(response) => response,
                Err(_) => return false,
            };
            let status = response.status();
            let payload = match response.json::<Value>().await {
                Ok(payload) => payload,
                Err(_) => return false,
            };
            let error_code = first_error_code(&payload);
            let retry = error_code == Some("Neo.ClientError.Transaction.ConcurrentRequestAccess");
            let gone = error_code == Some("Neo.ClientError.Transaction.TransactionNotFound");
            if !retry {
                return gone || (status.is_success() && error_code.is_none());
            }
            if let Some(delay) = BACKOFF_MS.get(attempt) {
                tokio::time::sleep(Duration::from_millis(*delay)).await;
            }
        }
        false
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

fn first_error_code(payload: &Value) -> Option<&str> {
    payload
        .get("errors")?
        .as_array()?
        .first()?
        .get("code")?
        .as_str()
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TxState {
    Open,
    Committing,
    Closed,
}

/// An explicit Neo4j transaction. Dropping an open transaction schedules a
/// best-effort DELETE so cancelled request futures do not retain database locks.
pub struct Tx {
    neo: Neo4j,
    commit_url: String,
    tx_url: String,
    state: TxState,
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

    pub async fn commit(self, statements: &[Statement]) -> ApiResult<Vec<QueryResult>> {
        let statements = statements.to_vec();
        tokio::spawn(async move { self.commit_inner(&statements).await })
            .await
            .map_err(|error| ApiError::storage(format!("neo4j commit task failed: {error}")))?
    }

    async fn commit_inner(mut self, statements: &[Statement]) -> ApiResult<Vec<QueryResult>> {
        // Once the commit request starts its outcome is ambiguous on
        // cancellation. Never issue a compensating DELETE or imply rollback.
        self.state = TxState::Committing;
        let result = self.neo.post(&self.commit_url.clone(), statements).await;
        self.state = TxState::Closed;
        Ok(result?.1)
    }

    /// Best-effort rollback: a failure here still leaves nothing committed.
    pub async fn rollback(mut self) {
        if self.state != TxState::Open {
            return;
        }
        if self.neo.delete_transaction(&self.tx_url).await {
            self.state = TxState::Closed;
        }
    }
}

impl Drop for Tx {
    fn drop(&mut self) {
        if self.state != TxState::Open {
            return;
        }
        self.state = TxState::Closed;
        let neo = self.neo.clone();
        let tx_url = self.tx_url.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = neo.delete_transaction(&tx_url).await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::State,
        http::{Method, StatusCode},
        response::IntoResponse,
        routing::any,
        Json, Router,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tokio::sync::{mpsc, Notify};

    #[derive(Clone)]
    struct ServerState {
        events: mpsc::UnboundedSender<Method>,
        hold_posts: Arc<Notify>,
        hold_deletes: Arc<Notify>,
    }

    async fn handler(State(state): State<ServerState>, method: Method) -> impl IntoResponse {
        state.events.send(method.clone()).unwrap();
        if method == Method::POST {
            state.hold_posts.notified().await;
        } else if method == Method::DELETE {
            state.hold_deletes.notified().await;
        }
        "{}"
    }

    async fn fixture() -> (Neo4j, mpsc::UnboundedReceiver<Method>, Arc<Notify>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (events, receiver) = mpsc::unbounded_channel();
        let hold_posts = Arc::new(Notify::new());
        let hold_deletes = Arc::new(Notify::new());
        let app = Router::new()
            .fallback(any(handler))
            .with_state(ServerState {
                events,
                hold_posts: hold_posts.clone(),
                hold_deletes,
            });
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let cfg = Config {
            bind: String::new(),
            neo4j_uri: format!("http://{address}"),
            neo4j_user: "neo4j".into(),
            neo4j_password: None,
            neo4j_database: "neo4j".into(),
            token: None,
            static_dir: String::new(),
        };
        (Neo4j::new(&cfg).unwrap(), receiver, hold_posts)
    }

    fn tx(neo: Neo4j, state: TxState) -> Tx {
        Tx {
            neo: neo.clone(),
            tx_url: format!("{}/tx/1", neo.base),
            commit_url: format!("{}/tx/1/commit", neo.base),
            state,
        }
    }

    #[tokio::test]
    async fn cancelling_in_flight_run_releases_open_transaction() {
        let (neo, mut events, _) = fixture().await;
        let task = tokio::spawn(async move { tx(neo, TxState::Open).run(&[]).await });
        assert_eq!(events.recv().await, Some(Method::POST));
        task.abort();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), events.recv())
                .await
                .unwrap(),
            Some(Method::DELETE)
        );
    }

    #[tokio::test]
    async fn cancelling_commit_does_not_claim_rollback() {
        let (neo, mut events, _) = fixture().await;
        let task = tokio::spawn(async move { tx(neo, TxState::Open).commit(&[]).await });
        assert_eq!(events.recv().await, Some(Method::POST));
        task.abort();
        assert!(
            tokio::time::timeout(Duration::from_millis(75), events.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn cancelling_rollback_retries_cleanup_from_drop() {
        let (neo, mut events, _) = fixture().await;
        let task = tokio::spawn(async move { tx(neo, TxState::Open).rollback().await });
        assert_eq!(events.recv().await, Some(Method::DELETE));
        task.abort();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), events.recv())
                .await
                .unwrap(),
            Some(Method::DELETE)
        );
    }

    async fn concurrent_then_deleted(
        State(attempts): State<Arc<AtomicUsize>>,
    ) -> impl IntoResponse {
        if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            (
                StatusCode::CONFLICT,
                Json(
                    json!({"errors":[{"code":"Neo.ClientError.Transaction.ConcurrentRequestAccess","message":"busy"}]}),
                ),
            )
        } else {
            (StatusCode::OK, Json(json!({"errors":[],"results":[]})))
        }
    }

    #[tokio::test]
    async fn cleanup_retries_exact_concurrent_access_even_on_409() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .fallback(any(concurrent_then_deleted))
            .with_state(attempts.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let cfg = Config {
            bind: String::new(),
            neo4j_uri: format!("http://{address}"),
            neo4j_user: "neo4j".into(),
            neo4j_password: None,
            neo4j_database: "neo4j".into(),
            token: None,
            static_dir: String::new(),
        };
        let neo = Neo4j::new(&cfg).unwrap();
        assert!(
            neo.delete_transaction(&format!("http://{address}/tx/1"))
                .await
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    #[ignore = "requires an isolated Neo4j configured by NEO4J_* environment variables"]
    async fn real_neo4j_cancelled_lock_is_released_without_seq_change() {
        let neo = Neo4j::new(&Config::from_env()).unwrap();
        let before = crate::store::read_context(&neo).await.unwrap().seq;
        let (acquired, ready) = tokio::sync::oneshot::channel();
        let holder_neo = neo.clone();
        let holder = tokio::spawn(async move {
            let mut tx = holder_neo.begin().await.unwrap();
            assert_eq!(crate::store::lock(&mut tx).await.unwrap(), before);
            let _ = acquired.send(());
            std::future::pending::<()>().await;
        });
        ready.await.unwrap();
        holder.abort();
        let _ = holder.await;

        let after = tokio::time::timeout(Duration::from_secs(5), crate::store::read_context(&neo))
            .await
            .expect("cancelled transaction lock must be released promptly")
            .unwrap()
            .seq;
        assert_eq!(
            after, before,
            "cleanup must not commit or advance domain seq"
        );

        // Also cancel a statement while it waits behind another transaction.
        // Keep the holder longer than the cleanup backoff budget so this
        // exercises Neo4j termination of an active, blocked HTTP statement.
        let mut blocker = neo.begin().await.unwrap();
        crate::store::lock(&mut blocker).await.unwrap();
        let waiting_neo = neo.clone();
        let (started, waiting) = tokio::sync::oneshot::channel();
        let waiter = tokio::spawn(async move {
            let mut tx = waiting_neo.begin().await.unwrap();
            let _ = started.send(());
            let _ = crate::store::lock(&mut tx).await;
            tx.rollback().await;
        });
        waiting.await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        waiter.abort();
        let _ = waiter.await;
        tokio::time::sleep(Duration::from_millis(700)).await;
        blocker.rollback().await;
        let after = tokio::time::timeout(Duration::from_secs(5), crate::store::read_context(&neo))
            .await
            .expect("cancelled blocked statement must not retain a subsequent lock")
            .unwrap()
            .seq;
        assert_eq!(after, before);
    }
}
