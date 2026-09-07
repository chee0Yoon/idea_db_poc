# idea_db work harness

Read `plan.md`, `docs/api.md` and the affected code before edits. Main owns orchestration, shared API decisions, integration and acceptance. Work in assigned files only; request a handoff for shared symbols. Do not invoke additional workers without Main approval.

## Invariants

- Neo4j is authoritative. No in-memory/SQLite success fallback.
- Stable application IDs; immutable revisions, observations and judgments.
- Recursive acyclic version-pinned composition, occurrence slot paths, isolated path edits.
- Atomic domain commit with expected-head conflict and digest-aware idempotency.
- Original captures survive failed downstream proposals.
- Scope assessment by exact root/path/baseline/evidence cutoff. AI proposals are visibly separate.
- Historical eligibility excludes future records. No mutating historical sources.
- Parameterized queries, bounded inputs/traversals, no client-supplied Cypher.
- Do not print provider secrets, commit `.env`, touch legacy projects, or delete unrelated Docker resources.

## Work contract

State owned files, required inputs, expected outputs and checks before delegation. Freeze shared contracts before parallel implementation. Worker completion is evidence, not acceptance. Main reviews changes and executes real Neo4j/container scenarios.

## Validation

`make check` must run formatting, Rust lint and domain tests. `make acceptance` must exercise the running API against real Neo4j. `make docker-check` must build/start the standalone image and prove persistence and restore. Exact commands and prerequisites are in README. Failed critical checks block release. Record measured outcomes and remaining limitations in `docs/validation.md`; never mark an unrun test passed.

## Definition of done

Implementation matches the documented API, smoke fixtures are reproducible without LLM keys, essential invariants pass meaningful tests, image has a readiness check and correct shutdown, fresh clone instructions work, Git diff contains no runtime secrets/data, and Main reports concrete evidence and limits.
