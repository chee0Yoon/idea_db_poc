//! idea_db: an immutable idea graph on Neo4j. The HTTP contract this crate
//! implements is `docs/api.md`; the internal split is `docs/internal-api.md`.

pub mod backup;
pub mod config;
pub mod error;
pub mod graph;
pub mod http;
pub mod limits;
pub mod model;
pub mod mutation;
pub mod neo4j;
pub mod query;
pub mod store;
pub mod util;
pub mod validate;
