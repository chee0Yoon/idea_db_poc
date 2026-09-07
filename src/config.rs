//! Runtime configuration from environment variables. See `docs/api.md` §1.

use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: String,
    pub neo4j_uri: String,
    pub neo4j_user: String,
    pub neo4j_password: Option<String>,
    pub neo4j_database: String,
    pub token: Option<String>,
    pub static_dir: String,
}

impl Config {
    pub fn from_env() -> Self {
        Config {
            bind: var_or("IDEA_DB_BIND", "127.0.0.1:8080"),
            neo4j_uri: var_or("NEO4J_URI", "http://127.0.0.1:7474")
                .trim_end_matches('/')
                .to_string(),
            neo4j_user: var_or("NEO4J_USER", "neo4j"),
            neo4j_password: non_empty("NEO4J_PASSWORD"),
            neo4j_database: var_or("NEO4J_DATABASE", "neo4j"),
            token: non_empty("IDEA_DB_TOKEN"),
            static_dir: var_or("STATIC_DIR", "./static"),
        }
    }
}

fn var_or(key: &str, default: &str) -> String {
    match env::var(key) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => default.to_string(),
    }
}

fn non_empty(key: &str) -> Option<String> {
    env::var(key).ok().filter(|v| !v.is_empty())
}
