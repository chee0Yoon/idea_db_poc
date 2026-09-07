# Rust 작업 경계 (v0.1 통합)

현재 공유 도메인 타입은 `src/model.rs`, `src/validate.rs`의 `Context`이다. API wire 계약은 `docs/api.md`이다.

## 파일 소유권

- Main: `model.rs`, `validate.rs`, `validate/tests.rs`, `util.rs`, `graph.rs`, `limits.rs`의 검토 결함 수정. 문서·Docker·scripts·최종 통합.
- Opus: `neo4j.rs`, 새 `store.rs` 등 저장/변경/백업 모듈, `main.rs`, `lib.rs`, HTTP 라우터, Cargo, examples. query.rs 구현을 중복하지 않는다.
- Terra: 새 `query.rs`와 해당 파일 내부 단위 테스트. static 화면 연동.
- Sol: `tests/acceptance.py`, 독립 검토.

## 조회 모듈 계약

조회 모듈은 Neo4j I/O 없이 **일관된 Context**만 읽는다. 호출자는 DB 트랜잭션 안에서 일관된 records/heads/seq를 읽어 전달한다.

```rust
pub type Params = std::collections::BTreeMap<String, String>;
pub fn state(ctx: &Context, params: &Params) -> ApiResult<Value>;
pub fn record(ctx: &Context, id: &str, params: &Params) -> ApiResult<Value>;
pub fn captures(ctx: &Context, params: &Params) -> ApiResult<Value>;
pub fn snapshot(ctx: &Context, params: &Params) -> ApiResult<Value>;
pub fn search(ctx: &Context, body: Value) -> ApiResult<Value>;
pub fn goals(ctx: &Context, params: &Params) -> ApiResult<Value>;
pub fn occurrences(ctx: &Context, params: &Params) -> ApiResult<Value>;
```

각 함수는 docs/api.md의 동명 조회 응답을 반환한다. 쿼리 문자열 시각/열거형/범위를 검증한다. `search`는 Value를 내부의 deny_unknown_fields 구조체로 해석한다. 조회 모듈이 필요한 헬퍼는 자기 파일 안에 둔다. 다른 작업자 소유 파일에 필드나 함수를 조용히 추가하지 않는다.

Main이 ObservationData.value를 `Option<serde_json::Value>`로 변경한다. 관측은 number/string/bool scalar를 지원하며 정량 기준 결과 `observed_value`는 기존 숫자형을 유지한다. 조회 모듈은 Value를 숫자로 단정하지 않는다.
