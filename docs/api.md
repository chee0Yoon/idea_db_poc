> **2026-09-07 MCP 전환:** 이 문서의 도메인 JSON 필드는 유지하며, 아래 기존 POST 경로 표기는 payload 설명을 위한 이전 명칭입니다. 해당 REST 쓰기 경로는 모두 405입니다. 변경은 MCP `idea_capture_create`, `idea_package_validate`, `idea_upload_preview`, `idea_upload_apply`, `idea_occurrence_replace`, `idea_import`만 사용합니다. 기존 apply payload는 preview의 `body.package`에 넣고, 실제 apply는 `body: {upload_id, prepared_digest}`를 받습니다. HTTP에는 GET과 읽기 전용 POST `/api/search`만 남습니다. 자세한 현행 전송 계약은 [mcp-intake.md](mcp-intake.md)입니다. MCP `tools/call` 도메인 실패는 `isError: true`, `structuredContent.error: {status,code,message,details}`입니다.

# idea_db API 계약 v1

### 추가 계약: 원자 요약과 검색 문맥 (2026-09-07)

- 모든 Revision은 선택적인 `summary: {text, source}`를 갖는다. `text`는 1..2000자, `source`는 기존 Source 형식이며 `origin: ai`, `claim_mode: inferred`, 모델 또는 스킬, 본문과 동일한 필수 capture_id를 요구한다. source_anchor는 허용하지 않는다. 생략한 과거 Revision의 JSON/digest는 바뀌지 않는다.
- 요약은 해당 본문/원문의 지시어·조건·예외를 풀어 쓴 검색 보조다. 공유 Idea에 사용 위치별 목표나 채택 판단을 주입하지 않는다. 의미 충실성은 클라이언트 검토 대상이며 DB 검증 성공이 이를 보증하지 않는다.
- 요약이 있는 Idea는 `Summary:\n{summary.text}\n\nBody:\n{body}` 전체를 로컬 임베딩한다. 프로파일은 `/idea-body-summary-v2`; 없는 Idea는 기존 본문과 `/idea-body-v1` 그대로다. 임베딩 ID는 실제 입력 전문·Revision·모델 프로파일에 결합한다. preview는 본문/요약과 검토 필요 여부를 반환한다.
- MCP 검색의 `embedding_format`은 `auto`(기본), `body_v1`, `body_summary_v2`다. auto는 동일한 고정 모델의 두 입력 형식을 명시적으로 허용하며, Revision별 최고 코사인 점수 한 개를 어휘 순위와 결합한다. 응답은 허용 프로파일 목록과 각 결과가 사용한 프로파일을 공개한다. 과거 벡터를 덮어쓰거나 다른 모델 digest를 혼합하지 않는다.
- 검색의 `context_budget_chars`는 0..32000, HTTP 기본 0 / MCP 기본 8000이다. 결과 전체 `occurrence_contexts` 패킷의 정규 JSON Unicode 문자 합계를 제한하며 응답 메타데이터·모델 토큰은 별도다. 사용 위치별 상위 Revision 요약/본문과 그 경로의 `goal_history`를 반환한다. 목표는 최신순 이력이며 `goal_selection: scope_history_not_active_baseline`으로 현재 기준선 선택과 구별한다. 기준선 상태는 `idea_goals`에서 조회한다. 각 상위 경로 자체의 역할과 root/path·시간 필터를 적용한다. 최대 128개 패킷·패킷당 16개 목표, 예산에 따른 본문/요약 축약과 생략을 표시한다. 요약은 원문을 대체하지 않으며 검색 점수/목표 적합성 판정에 이 문맥을 자동 합산하지 않는다.
- preview의 본문/요약 검토 표면은 `review_records`다. `embedding_inputs`에는 실제 입력 digest/프로파일/요약 포함 여부, `embedding_profiles`에는 실제 프로파일 목록이 있다. 기존 단수 `embedding_profile`은 호환을 위해 provider의 body-v1 기준값을 유지한다. occurrence replacement는 복사한 조상의 요약을 제거하며, 과거 요약을 소급 수정하거나 요약 추가만을 위한 가짜 본문 교정을 만들지 않는다.

상태: **고정(frozen), 개정 r2** · 2026-09-07 · 소유자: Opus(백엔드 구현) · 소비자: Main(통합/수용), UI 워커(`static/**`), 수용 워커(`tests/*.py`)

> **r2 변경 요약** (r1을 읽은 워커는 이 목록만 확인하면 된다)
> 1. 쓰기 직렬화를 프로세스 뮤텍스가 아니라 **Neo4j 트랜잭션 잠금**으로 한다(§2.6). 다중 프로세스 쓰기에서도 CAS가 성립한다.
> 2. **프로젝트 간 재사용 허용**. `entity.project_id`는 출처(origin)다. Schema만 자기 Project를 벗어난 구성에 들어갈 수 없다(§3.2, §3.3).
> 3. Project는 **여러 Schema 루트**를 가진다. Project 레코드를 `entity_id`로 갖는 **project-root Revision**을 도입한다(§3.3).
> 4. `change_kind: "composition"`을 클라이언트 패키지에서 허용한다(Idea 제외). `previous_revision_id` 추가(§3.3).
> 5. Capture는 **사건**이다. 자동 중복 제거를 없앴다(§5.5).
> 6. 외부 아티팩트는 이번 릴리스에서 **항상 미해결**이다. `included_in_export: true`는 거절한다(§3.7, §5.14).
> 7. `known_seq`/`known_at`이 `GET /api/records/{id}`에도 적용된다(§5.4).
> 8. Baseline `supersedes`가 **같은 범위의 다른 goal**을 넘어갈 수 있다(§3.10, §5.12).
> 9. 루트 변경은 **항상** `reason` + `decision`이 필수다. 의미성 추론 heuristic을 없앴다(§5.8).
> 10. 바이너리 이름은 `idea-db`다(§1).

이 문서는 구현보다 먼저 고정한 계약이다. 필드를 조용히 바꾸지 않는다. 변경이 필요하면 이 문서를 먼저 고치고 Main에 알린다.

- 프로토콜 버전: `1` (`protocol_version`)
- 전송: 변경은 MCP stdio JSON-RPC, 조회는 MCP 또는 HTTP JSON. MCP 프로토콜 버전과 도메인 `protocol_version: 1`은 별개다.
- 저장소: Neo4j 5.26 Community가 유일한 권위 저장소다. 메모리/파일 fallback은 없다.
- 시간: 모든 시각은 RFC 3339. `occurred_at`류 클라이언트 시각은 **오프셋 필수**(`Z` 또는 `+09:00`). 서버 시각 `recorded_at`은 UTC 밀리초(`2026-09-07T02:11:04.123Z`).

---

## 1. 실행 환경

| 환경변수 | 기본값 | 설명 |
|---|---|---|
| `IDEA_DB_BIND` | `127.0.0.1:8080` | 바인드 주소 |
| `NEO4J_URI` | `http://127.0.0.1:7474` | Neo4j HTTP 엔드포인트 |
| `NEO4J_USER` | `neo4j` | 사용자 |
| `NEO4J_PASSWORD` | (없음) | 비밀번호. 없으면 인증 헤더를 보내지 않는다 |
| `NEO4J_DATABASE` | `neo4j` | 데이터베이스 이름 |
| `IDEA_DB_TOKEN` | (없음) | 설정하면 `/api/*`에 `Authorization: Bearer <token>` 필수 |
| `STATIC_DIR` | `./static` | 정적 파일 루트. 런타임에 디스크에서 읽는다(`include_str!` 아님) |

실행 바이너리 이름은 `idea-db`다(`cargo run --bin idea-db`, 이미지 entrypoint `/usr/local/bin/idea-db`).

Neo4j 접근은 **HTTP 명시적 트랜잭션 API**(`POST /db/{db}/tx` → `POST /db/{db}/tx/{id}` → `POST /db/{db}/tx/{id}/commit`, 롤백은 `DELETE /db/{db}/tx/{id}`)를 쓴다. 이 엔드포인트는 5.26에서 deprecated지만 동작하며, 이번 MVP는 5.26.30 Community에 고정되어 있어 그대로 사용한다. Neo4j를 상위 버전으로 올릴 때 Query API v2로 교체해야 한다.

기동 시 Neo4j에 연결해 제약조건/인덱스를 만들고 싱글턴 잠금 노드를 `MERGE`한다. 실패하면 프로세스는 살아 있되 `/health/ready`가 503을 유지하고 5초마다 재시도한다. **준비되지 않은 동안 모든 `/api/*`는 503 `not_ready`를 반환한다.**

---

## 2. 공통 규칙

### 2.1 오류 형식

모든 오류는 아래 형태다. HTTP 상태는 표를 따른다.

```json
{
  "code": "validation_failed",
  "message": "revision rev_login_2: slot 'policy' references unknown revision rev_missing",
  "details": {
    "issues": [
      {"path": "records[3].data.slots[0].revision_id", "code": "unknown_reference", "message": "rev_missing not found"}
    ]
  }
}
```

| code | HTTP | 의미 |
|---|---|---|
| `bad_request` | 400 | JSON 파싱 실패, 알 수 없는 필드, 범위 위반 |
| `unauthorized` | 401 | `IDEA_DB_TOKEN` 불일치 |
| `not_found` | 404 | 레코드/경로 없음 |
| `payload_too_large` | 413 | 본문 크기 한계 초과 |
| `unsupported_media_type` | 415 | JSON 미디어 타입이 아닌 POST 본문 |
| `validation_failed` | 422 | 도메인 규칙 위반(참조/순환/출처/범위/의미편집 등) |
| `unsupported_protocol_version` | 422 | `protocol_version != 1` |
| `head_conflict` | 409 | `expected_heads` 불일치 |
| `idempotency_conflict` | 409 | 같은 키, 다른 본문 |
| `import_not_empty` | 409 | 비어 있지 않은 네임스페이스로 import |
| `digest_mismatch` | 422 | export 문서 digest 불일치 |
| `context_too_large` | 422 | 검증에 필요한 기존 레코드 수가 한계 초과 |
| `not_ready` | 503 | Neo4j 미연결 |
| `storage_error` | 502 | Neo4j 오류 |

알 수 없는 필드는 **신뢰 경계에서 실패한다**. 모든 요청 본문과 모든 레코드 `data`는 `deny_unknown_fields`다.

### 2.2 한계값

| 항목 | 한계 |
|---|---|
| 일반 POST 본문 | 2 MiB |
| import 본문 | 64 MiB |
| 패키지 레코드 수 | 500 |
| Revision 슬롯 수 | 64 |
| Revision `body` 길이 | 65536자 |
| Capture `content` 길이 | 262144자 |
| 슬롯 경로 깊이 / 탐색 깊이 | 32 |
| 스냅샷 `max_nodes` | 기본 500, 최대 5000 |
| 전체 네임스페이스 레코드 / 읽기·검증 Context | 5000 (초과 시 `context_too_large`) |
| 검색 `limit` | 기본 20, 최대 200 |
| 목록 `limit` | 기본 200, 최대 1000 |
| 임베딩 차원 | 1..=4096 |
| export 레코드 수 | 5000 |

**규모 트레이드오프(정직한 기록):** 쓰기는 §2.6의 DB 잠금으로 직렬화한다. 검색의 어휘/벡터 점수 계산과 패키지 검증은 경계 안의 후보 집합을 Rust 메모리로 가져와 정확히(exact) 계산한다. ANN 인덱스는 쓰지 않는다. 이 구조는 위 한계 안의 그래프에서 정확성을 우선한다. 팀 규모 성능을 입증한 것이 아니다.

### 2.3 식별자

- 모든 레코드의 `id`가 곧 응용 ID다. 전역 유일하며 Neo4j 유니크 제약으로 강제한다.
- 형식: `^[a-z][a-z0-9_.:-]{2,127}$`. 클라이언트가 부여한다(패키지 안에서 새 레코드끼리 참조해야 하므로).
- 서버가 만드는 ID(occurrence 복사본, capture 자동생성)는 접두사 + 16진수를 쓴다: `rev_a1b2...`, `cap_...`, `hc_...`, `pub_...`.
- 불변: 이미 존재하는 `id`를 다시 쓰면 `validation_failed`/`duplicate_id`다. 레코드는 절대 수정/삭제되지 않는다.

### 2.4 서버 부여 필드

모든 저장 레코드는 아래를 갖는다.

```json
{"id":"rev_login_1","kind":"revision","seq":12,"recorded_at":"2026-09-07T02:11:04.123Z","data":{ }}
```

- `seq`: 전역 커밋 순번(1부터 단조 증가). 한 패키지의 모든 레코드는 **같은 `seq`**를 갖는다(원자 배치 = 순번 1단위). 패키지 내부 순서는 `records` 배열 순서로 보존한다.
- `recorded_at`: 서버 기록 시각. `occurred_at`(실제 발생)과 구분한다.

### 2.5 정규 JSON과 digest

`canonical_json(x)` = 객체 키를 유니코드 코드포인트 오름차순으로 정렬하고 불필요한 공백이 없는 UTF-8 JSON. `digest(x) = "sha256:" + hex(sha256(canonical_json(x)))`.

멱등 키 본문 비교는 `request_digest = digest(요청 본문 전체)`를 쓴다.

### 2.6 쓰기 직렬화와 CAS (r2)

프로세스 뮤텍스만으로는 여러 클라이언트/여러 API 프로세스 사이의 CAS가 성립하지 않는다. 그래서 **Neo4j 트랜잭션 잠금**을 쓴다.

부트스트랩에서 싱글턴을 만든다.

```cypher
MERGE (m:IdeaDbMeta {id:'global'})
  ON CREATE SET m.seq = 0, m.lock_version = 0
```

쓰기가 있는 모든 요청(`packages/apply`, `occurrences/replace`, `captures`, `import`)은 트랜잭션의 **첫 구문**으로 이 노드에 쓰기를 걸고, 그 다음에야 멱등 키·head·기존 레코드를 읽는다.

```cypher
MATCH (m:IdeaDbMeta {id:'global'})
SET m.lock_version = m.lock_version + 1
RETURN m.seq AS seq, m.lock_version AS lock_version
```

Neo4j는 이 `SET`으로 노드 쓰기 잠금을 잡고 커밋/롤백까지 유지하므로, 같은 잠금을 노리는 다른 트랜잭션은 대기한다. **읽기보다 쓰기 의존을 먼저 거는 순서가 핵심이다.** 잠금을 잡은 뒤 읽은 head는 커밋 순간까지 유효하므로 `expected_heads` 비교가 곧 CAS다. 전역 `seq`도 이 잠금 아래에서 읽고 증가시킨다.

- 트랜잭션은 Rust 검증 전체와 쓰기, 커밋까지 살아 있다. 검증 전용(`packages/validate`)은 잠금을 잡되 반드시 롤백한다.
- Rust 프로세스 안의 뮤텍스는 불필요한 DB 경합을 줄이는 최적화로 유지하지만, 정확성은 DB 잠금이 담당한다.
- 잠금 대기가 트랜잭션 타임아웃을 넘기면 Neo4j가 실패를 돌려주고 서버는 `502 storage_error`로 전달한다. 부분 저장은 없다.

---

## 3. 레코드 종류

`kind` 값과 `data` 스키마. `*`는 필수, 나머지는 생략 시 기본값(배열은 `[]`, 나머지는 `null`).

### 3.1 `project`
```json
{"kind":"project","data":{"title":"로그인 개편","description":"2026 H2"}}
```
`title*`(1..200), `description`(0..4000).

### 3.2 `entity` — 안정 정체성 (Schema/Core/Idea)
```json
{"kind":"entity","data":{
  "entity_kind":"idea",
  "project_id":"proj_login",
  "title":"5회 실패 시 계정 잠금",
  "tags":["policy","auth"],
  "derived_from":["ent_lockout_v0"],
  "lineage_kind":"none"
}}
```
- `entity_kind*`: `schema` | `core` | `idea`
- `project_id*`: 존재하는 `project`. **출처(origin) Project**를 뜻한다. 엔티티의 출처는 하나로 고정되며 바뀌지 않는다.
  - **Schema**는 자기 출처 Project를 벗어난 구성에 들어갈 수 없다. 다른 Project의 project-root Revision이 그 Schema를 슬롯으로 고정하면 `schema_cross_project_composition`으로 거절한다.
  - **Core/Idea**는 같은 신뢰 소유자 아래에서 **다른 Project의 구성에 명시적으로 고정 참조될 수 있다**. 출처 Project는 계보/귀속 정보이지 재사용 금지 신호가 아니다. 스냅샷·검색·목표 범위는 **구성 소속(composition eligibility)** 으로 판정하며, 재사용된 자식을 출처 Project가 다르다는 이유로 제외하지 않는다.
  - 관측(`observation`)·아티팩트·캡처·후보는 원래대로 자기 `project_id` 문맥에 남는다. 공유 Idea에 붙은 관측이 다른 Project의 문맥으로 옮겨지지 않는다.
- `tags`: 확장 가능한 슬러그 `^[a-z0-9][a-z0-9_-]{0,31}$`, 최대 32개. **본문 주제 태그**이며 사용 위치의 역할(`roles`)과 다르다.
- `derived_from`: 기존 `entity` id 배열(최대 16). 의미 편집·분할·결합의 계보. `DERIVED_FROM` 관계로 물질화된다. 근거/평가는 자동 상속되지 않는다.
- `lineage_kind`: `none` | `semantic_edit` | `split` | `merge`. `derived_from`이 비어 있지 않으면 `none`이 아니어야 한다.

### 3.3 `revision` — 불변 내용 + 정확한 구성
```json
{"kind":"revision","data":{
  "entity_id":"ent_lockout",
  "body":"연속 5회 인증 실패 시 계정을 30분 잠근다.",
  "tags":["policy"],
  "slots":[{"slot_id":"policy","revision_id":"rev_lockout_1","roles":["be"]}],
  "change_kind":"initial",
  "correction_of":null,
  "correction_reason":null,
  "previous_revision_id":null,
  "source":{
    "origin":"human",
    "capture_id":"cap_meeting_0901",
    "model":null,
    "skill":null,
    "claim_mode":"extracted",
    "source_anchor":{"capture_id":"cap_meeting_0901","start":120,"end":168}
  }
}}
```
- `entity_id*`: 존재하는 `entity` **또는 `project` 레코드**. 후자를 **project-root Revision**이라 하며, 스냅샷에서 `entity_kind`는 `project`로 보고된다.
- `body*`: 1..65536자.
- `slots`: 순서 있는 배열, 최대 64. `slot_id*`는 슬러그, **한 Revision 안에서 유일**. `revision_id*`는 자식 Revision을 **정확히 고정**한다. `roles`는 사용 위치의 R&R 슬러그 배열(최대 8).
  - 같은 자식 Revision이 서로 다른 `slot_id`로 여러 번 나타나도 된다(다이아몬드).

**포함 규칙 (r2).** Project는 여러 Schema 루트를 가진다. 서로 관계없는 Schema를 묶는 가짜 상위 Schema를 만들지 않고, Project 자신을 루트 Revision으로 표현한다.

| 부모 Revision의 종류 | 허용되는 자식 |
|---|---|
| `project` (project-root) | `schema`만. 초안 Project는 `slots`가 비어도 된다 |
| `schema` | `core` \| `idea` (Schema 바로 아래 Idea 허용) |
| `core` | `core` \| `idea` |
| `idea` | 없음 (원자) |

`core`/`schema`가 `schema`나 `project`를 포함하면 `invalid_containment`다. Project의 working head와 공식 publication은 Project 자신을 `entity_id`로 갖는 project-root Revision이어야 한다. Schema/Core/Idea를 직접 Project head로 지정하면 `invalid_head_revision`이다.

- `change_kind*`:
  - `initial` — 해당 엔티티의 **첫** Revision. 이미 Revision이 있으면 거절한다(`initial_already_exists`). `correction_of`/`previous_revision_id`는 `null`.
  - `composition` — 같은 정체성의 구조/내용 새 버전. `project`/`schema`/`core` 엔티티에만 허용하며 **Idea에는 허용하지 않는다**(`composition_not_allowed_on_idea`). Idea의 내용 변경은 `correction`(교정) 또는 새 엔티티 + `semantic`(의미 변경)이다. 서버가 occurrence 복사로 만드는 조상 Revision도 같은 `composition`이다.
  - `correction` — **같은 엔티티**의 특정 기존 Revision을 고치는 오탈자/표현 교정. `correction_of*`(같은 엔티티의 기존 Revision을 **정확히 지정**)와 `correction_reason*`(1..500) 필수.
  - `semantic` — 의미 변경. 대상 엔티티는 **이 패키지에서 새로 만든 엔티티**여야 하고 `derived_from`이 비어 있지 않아야 한다. 기존 엔티티에 `semantic` Revision을 붙이면 거절한다.
- `previous_revision_id`: `composition`에서 선택. 주면 같은 엔티티의 기존 Revision이어야 한다. 생략하면 서버가 그 엔티티의 최신 Revision(`seq`, 동률 시 id)으로 해석하고 응답 receipt에 `resolved_previous`로 알린다. `initial`/`correction`/`semantic`에서는 `null`이어야 한다.

**교정 판정에 대한 정직한 한계.** `correction`을 받으면 서버가 이전/새 `body`의 숫자 토큰 다중집합을 비교해 다르면 거절한다(`semantic_edit_required`). `3초 → 30초`를 교정으로 통과시키지 않기 위한 **가드일 뿐, 의미 동등성의 증명이 아니다.** 숫자가 같아도 의미가 바뀌는 편집(부정어 추가, 조건 반전, 대상 교체)은 자동으로 잡히지 않는다. 그런 편집을 `correction`으로 넣지 않을 책임은 사람 검토자에게 있다. 응답의 `warnings`에 `correction_needs_human_review`를 항상 포함한다.

- `source*`:
  - `origin*`: `human` | `ai` | `import`
  - `claim_mode*`: `extracted` | `inferred`
  - `claim_mode == "extracted"`면 `source_anchor*` 필수이며 **앵커 구간의 문자열이 `body`와 정확히 같아야 한다**. `start`/`end`는 유니코드 스칼라(문자) 인덱스이고 `0 <= start < end <= 캡처 내용 문자 길이`다. 범위를 벗어나면 `source_anchor_out_of_range`, 문자열이 다르면 `source_anchor_text_mismatch`다. 원문을 바꿔 쓴 것(paraphrase)은 `extracted`가 아니라 `inferred`다.
  - `source.capture_id`가 있으면 `source_anchor.capture_id`와 같아야 한다(`source_capture_mismatch`).
  - `claim_mode == "inferred"`면 `source_anchor`는 `null`이어야 하고, 이 레코드는 추론으로 표시되어 검색/대시보드에서 구분된다.
  - `origin == "ai"`면 `model*` 또는 `skill*` 중 하나 이상 필요.

### 3.4 `capture` — 원문
```json
{"kind":"capture","data":{
  "project_id":"proj_login","content":"회의록 전문 …","content_digest":"sha256:…",
  "media_type":"text/plain","source_kind":"note","source_ref":null,
  "occurred_at":"2026-09-01T10:00:00+09:00"
}}
```
`source_kind*`: `paste` | `file` | `url` | `note` | `tool`. `content_digest`를 주면 서버 계산값과 **반드시 일치해야 한다**(`content_digest_mismatch`). 캡처는 `POST /api/captures`로 **독립 저장**되어 이후 패키지 검증이 실패해도 남는다. 캡처는 **사건**이므로 내용이 같아도 자동으로 합쳐지지 않는다(§5.5).

### 3.5 `candidate` — 후보(승격 전)
```json
{"kind":"candidate","data":{
  "project_id":"proj_login","capture_id":"cap_meeting_0901","proposed_kind":"idea",
  "title":"잠금 해제 경로 필요","body":"…","origin":"ai","model":"local-qwen3-8b","skill":"idea-db-input",
  "claim_mode":"extracted","source_anchor":{"capture_id":"cap_meeting_0901","start":300,"end":352},
  "status":"pending"
}}
```
`proposed_kind*`: `idea` | `core` | `schema` | `goal` | `link`. `status`는 생성 시 `pending`만 허용한다(레코드는 불변). 후보는 검색의 **별도 레인**으로만 노출되며 구성이나 공식 평가로 자동 승격되지 않는다.

### 3.6 `promotion` — 후보 승격 매핑
```json
{"kind":"promotion","data":{"candidate_id":"cand_1","entity_id":"ent_unlock","revision_id":"rev_unlock_1","actor":"human:cyyoon","reason":"검토 후 채택"}}
```
후보당 promotion은 하나다. `entity_id`/`revision_id`는 같은 패키지에서 새로 만든 것도 된다. `PROMOTES`/`PRODUCED` 관계로 원문·후보 연결이 남는다.

### 3.7 `artifact` — 외부 자료 참조
```json
{"kind":"artifact","data":{"uri":"file:///runs/2026-09-05/k6.json","digest":"sha256:…","digest_alg":"sha256","size_bytes":81234,"media_type":"application/json","included_in_export":false,"note":""}}
```
**이번 릴리스에서 외부 아티팩트는 항상 미해결이다.** 바이트를 받는 업로드 엔드포인트도, 보관소도 없다. 따라서 `included_in_export`는 `false`만 허용하며 `true`를 보내면 `422 artifact_bytes_unsupported`로 거절한다(클라이언트의 약속을 신뢰하지 않는다). 모든 아티팩트는 export manifest의 `unresolved_external_artifacts`에 나열되고, 아티팩트가 하나라도 있으면 `complete_backup`은 `false`다. 바이트 없는 내보내기를 완전한 백업이라고 표시하지 않는다.

### 3.8 `observation` — 관측 원자료
```json
{"kind":"observation","data":{
  "project_id":"proj_login","target_revision_id":"rev_lockout_1",
  "metric":"unlock_success_rate","value":0.62,"unit":"ratio","status":"observed",
  "occurred_at":"2026-09-05T14:00:00+09:00",
  "method":"A/B 2주, n=1,204",
  "environment":{"code_ref":"git:9f2c1a7","data_version":"login-2026-08","model_version":null,"config_digest":"sha256:…","runtime":"k6 0.52"},
  "artifact_ids":["art_k6_0905"],"capture_id":null,"actor":"human:cyyoon","note":""
}}
```
`status*`: `observed` | `failed` | `invalid` | `not_observed` | `negative`. `observed`이면 `value*`, `metric*` 필수. 그 외에는 `value`가 `null`이어야 한다. `occurred_at*` 오프셋 필수. `value`는 유한한 수, 문자열 또는 boolean을 허용하며 배열과 객체는 거절한다. 정성 관측도 원래 형태로 보존한다.

### 3.9 `goal` — 목표와 기준
```json
{"kind":"goal","data":{
  "project_id":"proj_login",
  "scope":{"root_revision_id":"rev_root_1","slot_path":["auth","policy"],"target_revision_id":"rev_lockout_1"},
  "statement":"잠금 정책이 계정 탈취를 막으면서 정상 사용자를 과도하게 막지 않는다",
  "criteria":[
    {"criterion_id":"c_takeover","kind":"quantitative","statement":"탈취 성공률","metric":"takeover_rate","comparator":"lte","threshold":0.001,"unit":"ratio","required":true},
    {"criterion_id":"c_ux","kind":"qualitative","statement":"CS 문의에서 잠금 불만이 주요 주제가 아니다","metric":null,"comparator":null,"threshold":null,"unit":null,"required":false}
  ],
  "origin":"official","actor":"human:cyyoon"
}}
```
- `scope*`: `root_revision_id*`에서 `slot_path`(빈 배열이면 루트 자신)를 따라간 결과가 `target_revision_id*`와 **정확히 같아야** 한다. `project_id`도 루트 Project와 일치해야 한다(`goal_project_mismatch`).
- `criteria*`: 1..32개. `criterion_id`는 goal 안에서 유일. `kind == "quantitative"`면 `metric/comparator/threshold` 필수, `comparator ∈ {lt,lte,gt,gte,eq,ne}`. `qualitative`면 셋 다 `null`.
- `origin*`: `official` | `ai_proposed`. AI 제안 목표는 공식 달성도에 섞이지 않는다.
- 목표는 모든 수준(Project 루트, Core, Idea의 사용 위치)에 붙일 수 있다. 없어도 되며 대시보드가 누락으로 표시한다.

### 3.10 `baseline` — 채택 기준선 고정
```json
{"kind":"baseline","data":{
  "goal_id":"goal_lockout","criterion_ids":["c_takeover","c_ux"],
  "constraints":["모바일 웹 제외","2026-09 트래픽 기준"],
  "effective_from":"2026-09-01T00:00:00+09:00",
  "supersedes":null,"actor":"human:cyyoon","reason":"1차 기준선 확정"
}}
```
`criterion_ids*`는 goal의 criterion을 부분집합으로 지정하며 goal의 `required: true` 기준은 모두 포함해야 한다.

**`supersedes` (r2).** 기준(threshold)을 바꾸려면 goal 자체가 새로 필요하다. goal은 불변이므로 `supersedes`는 **다른 goal의 baseline도 가리킬 수 있다.** 조건은 하나다: 이전 baseline이 속한 goal의 `scope`(`root_revision_id`, `slot_path`, `target_revision_id`)가 새 baseline이 속한 goal의 `scope`와 **정확히 같아야 한다**. 다르면 `baseline_supersede_scope_mismatch`로 거절한다. 이전 goal/baseline/assessment는 전혀 바뀌지 않고 이력으로 남는다.

**AI 제안 격리.** `origin == "ai_proposed"`인 goal에 걸린 baseline으로는 `origin == "official"`인 assessment를 만들 수 없다(`official_assessment_from_proposed_goal`). AI 제안 목표는 공식 달성도에 섞이지 않는다.

### 3.11 `assessment` — 불변 평가
```json
{"kind":"assessment","data":{
  "root_revision_id":"rev_root_1","slot_path":["auth","policy"],"target_revision_id":"rev_lockout_1",
  "baseline_id":"bl_lockout_1",
  "evidence_cutoff_seq":140,"evidence_cutoff_at":"2026-09-06T00:00:00+09:00",
  "evaluator":"human:cyyoon","rubric_version":"rubric-v1",
  "origin":"official","status":"unmet",
  "criteria_results":[
    {"criterion_id":"c_takeover","status":"met","observed_value":0.0004,"note":""},
    {"criterion_id":"c_ux","status":"unmet","observed_value":null,"note":"CS 문의 상위 3개 주제에 포함"}
  ],
  "evidence_observation_ids":["obs_takeover_0905"],
  "note":""
}}
```
검증:
1. `root_revision_id` + `slot_path` → `target_revision_id` 정확 일치.
2. `baseline_id`의 goal `scope`가 이 평가의 `root_revision_id`/`slot_path`/`target_revision_id`와 **완전히 같아야** 한다. 다르면 `baseline_scope_mismatch`.
3. 모든 근거 observation은 `seq <= evidence_cutoff_seq` **이고** `occurred_at <= evidence_cutoff_at`이어야 한다. 미래 근거는 거절한다(`evidence_after_cutoff`).
4. `criteria_results`의 `criterion_id`는 baseline의 `criterion_ids` 안이어야 하고, baseline이 참조하는 goal의 `required: true` 기준을 모두 덮어야 한다.
5. `status*`, 각 `criteria_results[].status*`: `met` | `unmet` | `unknown` | `disputed` | `recheck`.
6. `origin*`: `official` | `ai_proposed`. AI 평가는 대시보드에서 분리 표시하며 모델 confidence를 완료율로 쓰지 않는다.
7. 같은 criterion의 중복 결과는 거절한다. 전체 `status: met`는 필수 기준이 하나 이상이고 모두 `met`일 때만 허용하며 공식 `met`에는 관측 근거가 필요하다.
8. 근거 관측의 Project와 대상 Revision은 goal 범위와 일치해야 한다(`evidence_scope_mismatch`). 다른 문맥의 결과를 재사용하려면 현재 대상에 대한 적용 관측을 명시적으로 기록한다.

### 3.12 `link` — 유형 있는 의미 연결
```json
{"kind":"link","data":{
  "link_type":"impact","from_id":"ent_lockout","to_id":"goal_lockout",
  "note":"잠금 시간 단축이 CS 문의를 줄인다는 주장",
  "actor":"human:cyyoon",
  "impact":{"scope":"로그인 CS 문의","direction":"decrease","metric":"cs_tickets_per_1k","magnitude":18.0,"unit":"percent","evidence_kind":"estimated","observation_ids":[],"method":"과거 유사 정책 변경 비교 추정"},
  "similarity":null
}}
```

| `link_type` | 관계 타입 | 허용 from → to | 추가 규칙 |
|---|---|---|---|
| `derived` | `DERIVED` | entity → entity | |
| `applies` | `APPLIES` | entity → entity | 규칙을 적용하는 구현 |
| `implements` | `IMPLEMENTS` | entity → entity | FE/BE 구현은 각각 별도 Idea |
| `depends` | `DEPENDS` | entity → entity | |
| `similar` | `SIMILAR` | revision → revision | `similarity*` 필수. 동일성/인과의 증명이 아니다 |
| `support` | `SUPPORTS` | observation·capture·revision → revision·assessment·goal | |
| `contradict` | `CONTRADICTS` | observation·capture·revision → revision·assessment·goal | |
| `impact` | `IMPACTS` | entity·revision → goal | `impact*` 필수 |

`impact`: `direction ∈ {increase, decrease, neutral, unknown}`, `evidence_kind ∈ {estimated, observed}`. `observed`면 `observation_ids` 1개 이상 필수이며 모두 존재해야 한다. `estimated`면 `observation_ids`가 비어 있어야 하고 `method*` 필수. `similarity`: `score ∈ [-1,1]`, `method*`.

관계 타입은 위 8종 화이트리스트로만 물질화한다. 클라이언트 Cypher는 어떤 경로로도 받지 않는다.

### 3.13 `embedding` — 클라이언트 공급 벡터
```json
{"kind":"embedding","data":{"revision_id":"rev_lockout_1","model":"bge-m3","dim":1024,"values":[0.01,-0.02],"normalized":false}}
```
`dim == values.len()`. 서버는 어떤 임베딩 제공자도 호출하지 않는다. 검색 시 요청 벡터의 `model`/`dim`이 저장 벡터와 같아야 매칭 후보가 된다.

### 3.14 서버 전용 레코드

클라이언트는 `records`에 넣을 수 없다(`forbidden_kind`). 패키지의 `root_change`/`publish` 필드로 생성된다.

- `head_change`: `{"project_id","stage","before_revision_id","after_revision_id","reason","actor","meaningful","decision":{"before","after","rationale"}}`
- `publication`: `{"project_id","root_revision_id","label","published_at","notes","actor"}`

---

## 4. 그래프 물질화

레코드는 `:Record` + 종류별 라벨(`:Revision`, `:Entity`, …) 노드로 저장한다. `data`는 정규 JSON 문자열 프로퍼티이고, 검색·필터용 스칼라(`project_id`, `entity_id`, `entity_kind`, `revision_id`, `text`, `title`, `tags`, `status`, `link_type`, `origin`, `occurred_at_utc`, `seq`, `recorded_at`)를 함께 둔다.

물질화되는 관계(고정 타입만):

```
(:Revision)-[:CONTAINS {slot_id, roles, ord}]->(:Revision)
(:Revision)-[:REVISION_OF]->(:Entity)
(:Revision)-[:CORRECTION_OF]->(:Revision)
(:Entity)-[:IN_PROJECT]->(:Project)
(:Entity)-[:DERIVED_FROM]->(:Entity)
(:Candidate)-[:FROM_CAPTURE]->(:Capture)
(:Promotion)-[:PROMOTES]->(:Candidate)   (:Promotion)-[:PRODUCED]->(:Record)
(:Goal)-[:SCOPED_TO]->(:Revision)        (:Goal)-[:IN_PROJECT]->(:Project)
(:Baseline)-[:FOR_GOAL]->(:Goal)         (:Baseline)-[:SUPERSEDES]->(:Baseline)
(:Assessment)-[:AGAINST_BASELINE]->(:Baseline)  (:Assessment)-[:EVALUATES]->(:Revision)
(:Assessment)-[:EVIDENCE]->(:Observation)
(:Observation)-[:ARTIFACT]->(:Artifact)  (:Observation)-[:OBSERVES]->(:Revision)
(:Embedding)-[:EMBEDS]->(:Revision)
(:Record)-[:DERIVED|APPLIES|IMPLEMENTS|DEPENDS|SIMILAR|SUPPORTS|CONTRADICTS|IMPACTS {link_id}]->(:Record)
(:HeadChange)-[:IN_PROJECT]->(:Project)  (:HeadChange)-[:MOVES_TO]->(:Revision)
(:Publication)-[:PUBLISHES]->(:Revision) (:Publication)-[:IN_PROJECT]->(:Project)
```

`CONTAINS`는 `revision.data.slots`의 투영이다. 불일치 시 `data`가 권위다. Project 노드의 `working_head`/`working_head_seq`/`official_head`는 최신값 캐시이며, 이력은 `head_change`/`publication` 레코드가 갖는다.

---

## 5. 엔드포인트

### 5.1 `GET /health/live`
항상 `200 {"status":"live"}`. Neo4j를 보지 않는다.

### 5.2 `GET /health/ready`
```json
{"status":"ready","neo4j":{"uri":"http://127.0.0.1:7474","database":"neo4j","reachable":true},"seq":140,"protocol_version":1}
```
Neo4j 미연결이면 `503 {"code":"not_ready","message":"…","details":{"last_error":"…"}}`.

### 5.3 `GET /api/state`
쿼리: `project_id`(선택), `kinds`(쉼표 구분, 선택), `limit`(≤1000, 기본 200), `offset`(기본 0), `max_seq`(선택, 이 순번 이하만).

```json
{
  "seq": 140,
  "protocol_version": 1,
  "projects": [
    {"project_id":"proj_login","title":"로그인 개편","working_head":"rev_root_3","working_head_seq":138,
     "official_head":"rev_root_2","official_publication_id":"pub_v1","publication_count":1,
     "counts":{"entity":24,"revision":31,"capture":4,"candidate":6,"goal":3,"baseline":2,"assessment":2,"observation":5,"link":9,"artifact":1,"embedding":0,"promotion":2,"head_change":3,"publication":1}}
  ],
  "records": [{"id":"rev_root_3","kind":"revision","seq":138,"recorded_at":"…","data":{}}],
  "total_matched": 89,
  "truncated": false
}
```

### 5.4 `GET /api/records/{id}`
쿼리: `include`(쉼표 구분: `links`,`lineage`,`evidence`,`occurrences`; 기본 전부), `stage`(occurrences 계산용, 기본 `working`), **`known_seq` 또는 `known_at`**(선택), **`effective_at`**(선택).

**시점 필터는 상세 조회에도 적용된다 (r2).** `known_seq`/`known_at`을 주면 `seq > known_seq`인 레코드는 `revisions_of_entity`, `lineage`, `links`, `evidence`, `occurrences` 어디에도 나오지 않는다. `effective_at`을 주면 `occurred_at`이 그보다 미래인 관측/캡처가 `evidence`에서 빠진다. 평가도 `evidence_cutoff_at`과 연결된 기준선의 `effective_from`이 모두 조회 시점 이하여야 표시한다. 따라서 미래 근거로 만든 평가가 과거 관측 조회나 목표 상태에 간접적으로 새지 않는다. 요청한 레코드 자신이 `known_seq`보다 미래면 `404 not_found`다. 과거 시점 화면은 스냅샷·검색과 **같은 필터를 상세 조회에도 그대로 실어야** 한다. 필터를 생략하면 현재 시점(모든 레코드)이다.

```json
{
  "record": {"id":"rev_lockout_1","kind":"revision","seq":12,"recorded_at":"…","data":{}},
  "entity": {"id":"ent_lockout","kind":"entity","seq":11,"recorded_at":"…","data":{}},
  "revisions_of_entity": [{"id":"rev_lockout_1","seq":12,"change_kind":"initial","correction_of":null,"body_preview":"연속 5회…"}],
  "lineage": {"derived_from":[{"id":"ent_lockout_v0","title":"…","lineage_kind":"semantic_edit"}],"derives":[],"corrections":[]},
  "links": {"outgoing":[{"link_id":"lnk_1","link_type":"impact","to_id":"goal_lockout","data":{}}],"incoming":[]},
  "evidence": {"observations":[{"id":"obs_takeover_0905","data":{}}],"assessments":[{"id":"asm_1","data":{}}]},
  "occurrences": [{"root_revision_id":"rev_root_3","slot_path":["auth","policy"],"roles":["be"],"stage":"working"}],
  "truncated": false
}
```
없으면 404 `not_found`.

### 5.5 `POST /api/captures`
원문을 **독립 트랜잭션**으로 저장한다. 이후 어떤 패키지가 실패해도 남는다.

요청:
```json
{"id":"cap_meeting_0901","project_id":"proj_login","content":"회의록 전문 …","media_type":"text/plain","source_kind":"note","source_ref":null,"occurred_at":"2026-09-01T10:00:00+09:00","content_digest":null}
```
**캡처는 사건이다 (r2).** 같은 문장을 두 번 붙여넣은 것은 두 번의 사건이므로 내용 digest가 같다고 합치지 않는다.

- `id`를 **생략하면 항상 새 캡처**를 만든다. 내용이 완전히 같아도 별개 사건으로 남는다.
- `id`를 **명시하면** 그 id가 멱등 키다.
  - 같은 id + 정규화한 **전체 payload**가 같다 → 기존 캡처를 `"replay": true`로 그대로 반환(`200`). 재시도가 사건을 늘리지 않는다.
  - 같은 id + `content`/`source_ref`/`occurred_at`/`project_id`/`media_type`/`source_kind` 중 하나라도 다르다 → `409 idempotency_conflict`.
- 원문 바이트를 나중에 블롭 수준에서 공유·중복 제거하더라도 캡처 레코드(사건 발생 사실)를 지우지 않는다. `content_digest`는 같은 원문을 찾는 검색 키일 뿐 정체성이 아니다.

응답 `201`(신규) / `200`(재시도):
```json
{"capture":{"id":"cap_meeting_0901","kind":"capture","seq":7,"recorded_at":"…","data":{}},"content_digest":"sha256:…","replay":false}
```

### 5.6 `GET /api/captures`
쿼리: `project_id`, `limit`(≤200, 기본 50), `offset`, `include_content`(기본 `false` → `content_preview` 400자만).
```json
{"captures":[{"id":"cap_meeting_0901","seq":7,"recorded_at":"…","occurred_at":"…","source_kind":"note","content_digest":"sha256:…","content_length":1820,"content_preview":"…","content":null,"candidate_count":3}],"total_matched":4,"truncated":false}
```

### 5.7 `POST /api/packages/validate`
`apply`와 **완전히 같은 요청 본문**을 받아 검증만 한다. 쓰기 없음. 트랜잭션은 롤백한다.
```json
{"valid":false,
 "errors":[{"path":"records[2].data.slots[1].revision_id","code":"unknown_reference","message":"rev_missing not found"}],
 "warnings":[{"path":"records[5]","code":"missing_goal","message":"새 Core에 목표가 없다"}],
 "derived":{"meaningful_root_change":true,"records_to_write":6,"next_seq":141,"context_records_loaded":42}}
```
검증 실패여도 HTTP `200`이다(`valid:false`). 요청 자체가 깨졌으면 `400`.

### 5.8 `POST /api/packages/apply`

요청:
```json
{
  "protocol_version": 1,
  "idempotency_key": "pkg-2026-09-07-001",
  "actor": "human:cyyoon",
  "reason": "로그인 정책 초기 등록",
  "expected_heads": [{"project_id":"proj_login","stage":"working","revision_id":null}],
  "records": [
    {"id":"proj_login","kind":"project","data":{"title":"로그인 개편","description":""}},
    {"id":"ent_lockout","kind":"entity","data":{"entity_kind":"idea","project_id":"proj_login","title":"5회 실패 시 잠금","tags":["policy"],"derived_from":[],"lineage_kind":"none"}},
    {"id":"rev_lockout_1","kind":"revision","data":{"entity_id":"ent_lockout","body":"연속 5회 인증 실패 시 계정을 30분 잠근다.","tags":[],"slots":[],"change_kind":"initial","correction_of":null,"correction_reason":null,"source":{"origin":"human","capture_id":"cap_meeting_0901","model":null,"skill":null,"claim_mode":"extracted","source_anchor":{"capture_id":"cap_meeting_0901","start":120,"end":168}}}}
  ],
  "root_change": {"project_id":"proj_login","stage":"working","after_revision_id":"rev_root_1","reason":"로그인 스키마 최초 구성","meaningful":true,"decision":{"before":"루트 없음","after":"auth 스키마 도입","rationale":"정책을 재사용 단위로 분리하기 위해"}},
  "publish": null
}
```

- `expected_heads*`: 이 패키지가 건드리는 모든 Project에 대해 CAS. `revision_id: null`은 "아직 head 없음"을 뜻한다. 불일치 시 `409 head_conflict`.
- `stage`: `working`만 `root_change`에서 허용한다. 공식 반영은 `publish`가 담당한다.
- **`root_change`는 언제나 `meaningful: true` + `reason`(1..500) + `decision`(`before`, `after`, `rationale` 1..1000)을 요구한다 (r2).** 데이터 모양으로 의미성을 추론하던 heuristic을 없앴다. 역할(`roles`) 재배정, 슬롯 순서 변경, 숫자가 아닌 의미 있는 문장 수정 같은 변경을 자동 분류가 놓치면 근거 없는 "사소한 변경" 표시가 남는데, 그 위험이 매번 한 줄 사유를 적는 부담보다 크다. `meaningful: false`는 `422 root_change_requires_decision`이다.
- `publish`: `{"project_id","root_revision_id","label","published_at","notes"}`. 공식 publication을 만든다. `published_at`은 오프셋 필수.

동작 순서(중요):
1. Neo4j 트랜잭션 시작 → **첫 구문으로 §2.6 잠금 획득**(쓰기 의존을 읽기보다 먼저 건다).
2. `idempotency_key` 조회. **head 비교보다 먼저 한다.**
   - 있고 `request_digest`가 같다 → 저장된 receipt를 `replay: true`로 그대로 반환(`200`). head가 그 사이 움직였어도 `409`가 아니다.
   - 있고 다르다 → `409 idempotency_conflict`.
3. `expected_heads` CAS 비교 → 불일치면 `409 head_conflict` + 롤백.
4. 참조 폐포(closure) prefetch → Rust에서 전 규칙 검증.
5. 새 레코드 + 관계 + head 델타 + receipt를 같은 트랜잭션에 기록하고 commit.
6. 어떤 단계든 실패하면 트랜잭션을 롤백한다. **부분 저장은 없다.**

응답 `200`:
```json
{"receipt":{
  "idempotency_key":"pkg-2026-09-07-001",
  "request_digest":"sha256:…",
  "applied_at":"2026-09-07T02:11:04.123Z",
  "seq":141,
  "replay":false,
  "records_written":["proj_login","ent_lockout","rev_lockout_1","hc_9a3f…"],
  "heads":[{"project_id":"proj_login","stage":"working","revision_id":"rev_root_1","seq":141}],
  "publication_id":null
}}
```

### 5.9 `POST /api/occurrences/replace`
한 사용 위치만 바꾸는 편의 엔드포인트. **선택 경로의 조상만 복사**하고 나머지 사용 위치는 기존 Revision에 그대로 고정된다. 6단계 이상 깊이와 다이아몬드 재사용에서도 수정 위치가 모호하지 않다.

요청:
```json
{
  "protocol_version": 1,
  "idempotency_key": "occ-2026-09-07-004",
  "actor": "human:cyyoon",
  "reason": "BE 경로의 잠금 시간만 조정",
  "project_id": "proj_login",
  "stage": "working",
  "expected_root_revision_id": "rev_root_3",
  "slot_path": ["auth","session","policy"],
  "replacement": {
    "mode": "new_revision",
    "new_revision": {"id":"rev_lockout_2","data":{"entity_id":"ent_lockout_v2","body":"연속 5회 인증 실패 시 계정을 10분 잠근다.","tags":[],"slots":[],"change_kind":"semantic","correction_of":null,"correction_reason":null,"source":{"origin":"human","capture_id":null,"model":null,"skill":null,"claim_mode":"inferred","source_anchor":null}}},
    "new_records": [{"id":"ent_lockout_v2","kind":"entity","data":{"entity_kind":"idea","project_id":"proj_login","title":"5회 실패 시 10분 잠금","tags":["policy"],"derived_from":["ent_lockout"],"lineage_kind":"semantic_edit"}}],
    "target_revision_id": null
  },
  "root_change_reason": "잠금 시간 정책 변경",
  "decision": {"before":"30분 잠금","after":"10분 잠금","rationale":"CS 문의 급증, 탈취율 여유 있음"}
}
```
- `replacement.mode`: `new_revision`(새 Revision 정의) | `existing_revision`(`target_revision_id`로 기존 Revision 지정). 둘 중 해당하지 않는 필드는 `null`이어야 한다.
- `new_records`: 새 Revision이 필요로 하는 부수 레코드(대개 새 `entity`). 패키지와 같은 규칙으로 검증한다.
- `slot_path`는 루트에서 시작하며 빈 배열은 허용하지 않는다(루트 교체는 패키지를 쓴다).
- 서버가 경로 조상 Revision을 **새 id로 복사**한다. 복사본 id는 `"rev_" + hex16(sha256(idempotency_key + ":" + 원본_revision_id))`로 결정적이다. 복사본은 `change_kind: "composition"`으로 기록되고, 바뀐 슬롯만 새 자식을 가리키며 다른 슬롯은 원래 Revision id를 그대로 유지한다.
- `root_change_reason*`과 `decision*`은 **항상 필수**다(§5.8과 같은 규칙).

응답 `200`:
```json
{"receipt":{"idempotency_key":"occ-2026-09-07-004","request_digest":"sha256:…","applied_at":"…","seq":142,"replay":false,
  "records_written":["ent_lockout_v2","rev_lockout_2","rev_a91c…","rev_77b0…","rev_4e12…","hc_…"],
  "heads":[{"project_id":"proj_login","stage":"working","revision_id":"rev_4e12…","seq":142}],"publication_id":null},
 "new_root_revision_id":"rev_4e12…",
 "mapping":[{"slot_path":[],"old_revision_id":"rev_root_3","new_revision_id":"rev_4e12…"},
            {"slot_path":["auth"],"old_revision_id":"rev_auth_1","new_revision_id":"rev_77b0…"},
            {"slot_path":["auth","session"],"old_revision_id":"rev_sess_1","new_revision_id":"rev_a91c…"},
            {"slot_path":["auth","session","policy"],"old_revision_id":"rev_lockout_1","new_revision_id":"rev_lockout_2"}],
 "pinned_unchanged":[{"slot_path":["auth","login_form"],"revision_id":"rev_form_1"},
                     {"slot_path":["billing","policy"],"revision_id":"rev_lockout_1"}]}
```
`pinned_unchanged`는 같은 원자를 쓰는 **다른 사용 위치가 그대로임**을 증명하는 목록이다(경계 안에서 나열, 초과 시 `truncated: true`).

### 5.10 `GET /api/snapshot`
쿼리: `project_id*`, `stage`(`working`|`official`, 기본 `working`), `known_seq` 또는 `known_at`(선택), `effective_at`(선택), `max_nodes`(≤5000, 기본 500), `depth`(≤32, 기본 32), `root_revision_id`(선택, 명시하면 head 해석을 건너뛴다).

시점 해석:
- `known_at`을 주면 `recorded_at <= known_at`인 최대 `seq`를 `known_seq`로 삼는다. 둘 다 없으면 현재 `seq`.
- `stage=working`: `seq <= known_seq`인 `head_change`(stage=working) 중 `seq` 최대값의 `after_revision_id`.
- `stage=official`: `seq <= known_seq`이고 `published_at <= effective_at`(생략 시 무한)인 `publication` 중 **`(published_at, seq, id)` 사전식 최대**. 결정적 우선순위다.
- 미래 레코드(`seq > known_seq`)와 미래 근거(`occurred_at > effective_at`)는 어디에도 새지 않는다. 평가의 근거 종료 시각(`evidence_cutoff_at`)이나 기준선 시작 시각이 미래라면 그 평가는 목표 상태와 `stale_assessments`에서도 제외한다.

응답:
```json
{
  "project_id":"proj_login","stage":"working",
  "selected_by":{"known_seq":140,"known_at":null,"effective_at":null,"head_change_id":"hc_3","publication_id":null},
  "root_revision_id":"rev_root_3",
  "tree":{"revision_id":"rev_root_3","slot_id":null,"slot_path":[],"roles":[],"depth":0,
          "children":[{"revision_id":"rev_auth_1","slot_id":"auth","slot_path":["auth"],"roles":["be"],"depth":1,"children":[]}]},
  "nodes":[{"revision_id":"rev_root_3","entity_id":"ent_root","entity_kind":"schema","title":"로그인 스키마","body":"…","tags":[],"change_kind":"composition","seq":138,"recorded_at":"…","occurrence_count":1}],
  "node_count":12,"revision_count":9,"max_depth_reached":6,"truncated":false
}
```
`tree`는 사용 위치(occurrence) 관점이라 같은 `revision_id`가 여러 번 나타날 수 있다. `nodes`는 Revision별로 한 번씩만 담는다. `max_nodes` 초과 시 `truncated: true`이며 그 지점 이후 `children`은 비어 있고 `"elided": true`가 붙는다. head가 없으면 `root_revision_id: null`, `tree: null`.

### 5.11 `POST /api/search`
```json
{
  "project_id":"proj_login","query":"계정 잠금 시간",
  "stage":"working","known_seq":null,"known_at":null,"effective_at":null,
  "roles":["be"],
  "tags":[],
  "lanes":["official","candidate"],
  "limit":20,
  "vector":{"model":"bge-m3","dim":1024,"values":[0.01,-0.02]}
}
```
- 대상은 선택 스냅샷에 **속한(eligible)** Revision 본문이다. 스냅샷 밖/미래 Revision은 대상이 아니다.
- `roles`: 사용 위치의 역할에 대한 **엄격한 하드 필터**. 하나라도 맞아야 `results`에 들어간다. 역할이 안 맞지만 링크로 이어진 노드(공유 정책 등)는 `related`에 **별도로** 나온다.
- 어휘 검색: 본문/제목/태그를 유니코드 단어 및 2-gram(CJK)으로 토큰화해 경계 안 후보에 대해 정확 점수를 계산한다.
- 벡터 검색: `vector`를 주면 같은 `model`·`dim`의 저장 `embedding`에 대해 **정확 코사인**을 계산한다. ANN 인덱스는 없다. 모델/차원이 하나도 안 맞으면 벡터 레인은 비고 `vector_status.used: false`로 표시한다.
- 융합: RRF(`k = 60`), `score = Σ 1/(60 + rank)`.
- 임베딩이 아예 없으면 어휘 검색만으로 정상 동작하고 상태를 표시한다.
- 후보 레인(`candidate`)은 항상 `candidates` 배열로 **분리**되어 정식 결과에 섞이지 않는다.

응답:
```json
{
  "mode":"lexical+vector","stage":"working",
  "selected_by":{"known_seq":140,"root_revision_id":"rev_root_3"},
  "results":[{"revision_id":"rev_lockout_1","entity_id":"ent_lockout","entity_kind":"idea","title":"5회 실패 시 잠금",
    "snippet":"연속 5회 인증 실패 시 계정을 **30분** 잠근다.","tags":["policy"],
    "occurrences":[{"slot_path":["auth","session","policy"],"roles":["be"]}],
    "lexical_rank":1,"vector_rank":2,"score":0.0325,"claim_mode":"extracted","origin":"human"}],
  "related":[{"revision_id":"rev_form_1","entity_id":"ent_form","title":"로그인 폼","reason":"role_mismatch","roles":["fe"],"link_type":null}],
  "candidates":[{"candidate_id":"cand_7","title":"잠금 해제 경로 필요","body_preview":"…","origin":"ai","model":"local-qwen3-8b","status":"pending","lexical_rank":1,"score":0.0164}],
  "vector_status":{"requested":true,"model":"bge-m3","dim":1024,"eligible_revisions":9,"with_matching_embedding":6,"used":true},
  "eligible_revisions":9,"truncated":false
}
```
과거 시점 검색의 **순위 재현까지는 보장하지 않는다**(대상 집합의 시점 정확성만 보장한다).

### 5.12 `GET /api/goals`
쿼리: `project_id*`, `stage`, `known_seq`/`known_at`, `effective_at`, `root_revision_id`(선택), `max_nodes`.

```json
{
  "scope":{"project_id":"proj_login","stage":"working","known_seq":140,"root_revision_id":"rev_root_3"},
  "goals":[{
    "goal_id":"goal_lockout","statement":"…","origin":"official",
    "scope":{"slot_path":["auth","session","policy"],"target_revision_id":"rev_lockout_1","in_current_snapshot":true},
    "origin_project_id":"proj_login",
    "criteria":[{"criterion_id":"c_takeover","kind":"quantitative","required":true,"statement":"탈취 성공률","metric":"takeover_rate","comparator":"lte","threshold":0.001,"unit":"ratio"}],
    "baselines":[{"baseline_id":"bl_lockout_1","effective_from":"…","supersedes":null,"criterion_ids":["c_takeover","c_ux"],
      "official_assessment":{"assessment_id":"asm_1","status":"unmet","evaluator":"human:cyyoon","rubric_version":"rubric-v1","recorded_at":"…","evidence_cutoff_seq":140,
        "scope_match":"exact","stale":false,
        "criteria_results":[{"criterion_id":"c_takeover","status":"met","observed_value":0.0004,"required":true}]},
      "superseded_by":null,
      "proposed_assessments":[{"assessment_id":"asm_ai_1","status":"met","evaluator":"ai:local-qwen3-8b","origin":"ai_proposed"}]}],
    "gate_status":"unmet",
    "required_criteria_status":[{"criterion_id":"c_takeover","status":"met","required":true}]
  }],
  "stale_assessments":[{"assessment_id":"asm_0","goal_id":"goal_lockout","baseline_id":"bl_lockout_0","reason":"root_not_in_snapshot","status":"met","recorded_at":"…"}],
  "missing_goal_occurrences":[{"slot_path":["auth","login_form"],"revision_id":"rev_form_1","entity_id":"ent_form","entity_kind":"idea"}],
  "proposed_goals":[{"goal_id":"goal_ai_1","statement":"…","origin":"ai_proposed"}],
  "note":"gate_status는 baseline의 required 기준으로만 결정한다. 원자 개수 평균을 진척률로 쓰지 않는다.",
  "truncated":false
}
```
`gate_status`: baseline의 `required` 기준이 모두 `met`이면 `met`, 하나라도 `unmet`이면 `unmet`, `disputed`가 있으면 `disputed`, 그 외 `unknown`. AI 예상 달성률은 `criteria_results[].progress_estimate`로 별도 제공하며 공식 gate와 섞지 않는다. 자식 완료는 부모 통합 목표 충족을 뜻하지 않으므로 상위 goal의 상태는 자식에서 파생하지 않고 그 goal의 assessment만 본다.

**Project·Schema·재귀 Core 목표와 AI 예상 달성률.** 목표와 평가는 각 수준의 정확한 root/slot_path/target에 둔다. 현재 목표가 누락되었으면 로컬 AI가 그 수준 자체의 목표와 필수 기준을 제안한다. 부모 목표의 평가는 연결된 아이디어와 근거를 대조해 별도로 작성하며 자식 개수나 자식 완료율을 자동 평균하지 않는다.

`CriterionResult`는 다음 선택 필드를 지원한다. 없으면 직렬화에서 생략하므로 기존 기록/export digest는 유지된다.

```json
{"criterion_id":"source_review","status":"unknown","observed_value":null,
 "note":"성능 지표는 아직 미측정",
 "progress_estimate":{"percent":20,"rationale":"요구가 현재 원자 계획에 반영되었으나 구현·검증 근거는 없다.",
 "evidence_record_ids":["capture_current_plan","revision_current_idea"]}}
```

- `progress_estimate`는 `origin=ai_proposed` 평가에서만 허용한다. `percent`는 유한한 0..100,
  `rationale`은 공백이 아닌 1..2000자, 근거 ID는 중복 없이 1..64개다.
- 근거는 같은 Project의 Capture/Revision/Observation이며 기록 seq와 recorded_at이 평가 cutoff 이하이어야 한다.
  Observation의 occurred_at도 cutoff 이하이다. 다른 Project에서 재사용한 Revision의 근거는
  평가 Project에 속한 적용 가능성 Capture/Observation으로 설명한다. Artifact는 Observation을 통해 참조한다.
- 기존 `observed_value`, `status`, `gate_status`는 예상 퍼센트에서 생성하지 않는다. 수치 기준이 미측정이어도
  기획·구현 단계에 대한 AI 추정은 존재할 수 있다. 공식 목표에도 별도 `proposed_assessments`를 붙일 수 있다.
- `rubric_version=goal-progress-milestones-v1`이면 기준별 점수는 0/20/40/60/80/100만 허용한다.
  각각 목표에 맞는 계획 미확인/원자 계획/적용 가능한 상세 설계/적용 가능한 구현/현재 범위의 부분 검증/기준 검증 완료이다.
  다른 명시적 rubric은 0..100을 사용하며 산정 방법을 평가 note에 설명한다.
- 대시보드는 활성 기준선에 채택된 필수 기준 전부에 추정이 있을 때만 같은 비중의 평균을 그 목표의
  AI 예상 달성률로 표시한다. 일부 누락이면 산정 불가와 평가 범위를 표시하고 0으로 대체하지 않는다.
  Project/Schema/Core 자체 목표와 하위 목표는 별도 표시한다. 성공 확률이나 투입 시간 비율이 아니다.
- 목표 조회의 평가에는 `evidence_cutoff_at`, `note`, `evidence_observation_ids`도 포함한다.
  최신 평가/기준선에 추정이 없으면 과거 추정을 자동 계승하지 않는다.

**범위 불일치 처리 (r2).** assessment의 `root_revision_id`/`slot_path`/`target_revision_id`가 현재 스냅샷에서 그대로 해석되지 않으면 그 평가를 **조용히 현재 상태로 쓰지 않는다.** `official_assessment`에서 제외하고 `stale_assessments`에 `reason`(`root_not_in_snapshot` | `path_not_resolvable` | `target_changed`)과 함께 남긴다. 그 baseline의 `gate_status`는 `unknown`이 되며 재검토가 필요하다는 뜻이다. 이력은 지우지 않는다. `superseded_by`는 이 baseline을 `supersedes`로 가리키는 최신 baseline id다(다른 goal에 속할 수 있다).

### 5.13 `GET /api/occurrences`
쿼리: `project_id*`, `entity_id` 또는 `revision_id` 중 하나*, `stage`, `known_seq`/`known_at`, `max_nodes`.
```json
{"selected_by":{"known_seq":140,"root_revision_id":"rev_root_3"},
 "occurrences":[{"slot_path":["auth","session","policy"],"revision_id":"rev_lockout_1","roles":["be"],"parent_revision_id":"rev_sess_1","depth":3}],
 "occurrence_count":2,"truncated":false}
```

### 5.14 `GET /api/export`
쿼리: `project_id`(생략 시 **네임스페이스 전체**), `include_receipts`(기본 `true`).

**네임스페이스 전체 export가 기준이다.** `project_id`를 주면 프로젝트 간 재사용 때문에 단순 필터가 참조를 깨뜨릴 수 있으므로, 서버는 그 프로젝트의 레코드에서 시작해 **참조 폐포 전체**(재사용된 다른 Project의 Core/Idea 엔티티와 Revision, 그리고 그 엔티티들의 출처 `project` 레코드 포함)를 함께 담는다. 폐포가 §2.2 한계를 넘으면 `422 export_scope_unsupported`로 **명시적으로 거절한다**(조용히 잘라내지 않는다). 이때는 전체 export를 쓴다. 프로젝트 범위 export의 `heads`에는 그 프로젝트의 head만 담기므로, 같이 담긴 출처 Project는 head 없는 참조용이다.

```json
{
  "format":"idea_db.export",
  "format_version":1,
  "protocol_version":1,
  "exported_at":"2026-09-07T03:00:00.000Z",
  "scope":{"project_id":null,"closure_included":true},
  "digest_alg":"sha256",
  "digest":"sha256:…",
  "content":{
    "seq":142,
    "records":[{"id":"proj_login","kind":"project","seq":1,"recorded_at":"…","data":{}}],
    "heads":[{"project_id":"proj_login","working_head":"rev_4e12…","working_head_seq":142,"official_head":"rev_root_2","official_publication_id":"pub_v1"}],
    "receipts":[{"idempotency_key":"pkg-2026-09-07-001","request_digest":"sha256:…","seq":141,"recorded_at":"…","response":{}}],
    "manifest":{
      "external_artifacts":[{"artifact_id":"art_k6_0905","uri":"file:///runs/2026-09-05/k6.json","digest":"sha256:…","included_in_export":false}],
      "unresolved_external_artifacts":["art_k6_0905"],
      "complete_backup":false
    }
  }
}
```
`digest`는 `canonical_json(content)`의 SHA-256이다. `records`는 `(seq, 배열 내 원래 순서)` 오름차순으로 정렬해 순서를 보존한다. `complete_backup`은 `external_artifacts`가 **비었을 때만** `true`다. 이번 릴리스는 아티팩트 바이트를 담지 않으므로 아티팩트가 하나라도 있으면 언제나 `false`다. **외부 파일이 빠진 내보내기를 완전한 백업으로 표시하지 않는다.**

### 5.15 `POST /api/import`

요청 본문은 export 문서를 `document` 키로 **감싼다**. CLI/스크립트는 `jq '{document: .}' backup.json` 형태로 감싸서 보낸다.

```json
{"document":{ /* 5.14 응답 전체 */ }}
```
- 대상 네임스페이스에 `:Record` 노드가 하나라도 있으면 `409 import_not_empty`. 빈 DB로만 복원한다.
- `digest` 재계산 후 불일치면 `422 digest_mismatch`.
- 저장 전에 **모든 불변 조건을 다시 검증한다**(참조, 순환, 슬롯, 출처 앵커, 목표/기준선 범위, 근거 cutoff, 링크 타입 규칙, id 유일성).
- 검증을 통과하면 하나의 트랜잭션으로 저장하며 `id`, `seq`, `recorded_at`, `occurred_at`, 배열 순서, head, receipt를 **그대로 보존한다**. 새 순번을 부여하지 않는다.

응답 `200`:
```json
{"imported":{"records":142,"heads":1,"receipts":3,"seq":142},
 "digest":"sha256:…",
 "unresolved_external_artifacts":["art_k6_0905"],
 "complete_backup":false}
```

### 5.16 정적 파일
`/api/*`, `/health/*`에 걸리지 않은 GET은 `STATIC_DIR`에서 제공한다. 디렉터리 요청과 미매칭 경로는 `index.html`로 폴백한다(SPA 라우팅). `STATIC_DIR`이 없거나 파일이 없으면 `404 {"code":"not_found",…}`. 정적 파일은 인증 대상이 아니다.

---

## 6. 검증 규칙 코드

`details.issues[].code`로 나오는 값이다. UI/수용 테스트는 이 코드를 안정 계약으로 쓸 수 있다.

| code | 의미 |
|---|---|
| `duplicate_id` | 이미 존재하거나 패키지 안에서 중복된 id |
| `bad_id_format` | id 형식 위반 |
| `unknown_reference` | 참조 대상 없음 |
| `wrong_reference_kind` | 참조 대상의 종류가 규칙과 다름 |
| `cycle_detected` | CONTAINS 순환 |
| `self_reference` | 자기 자신을 슬롯으로 포함 |
| `duplicate_slot_id` | 한 Revision 안 slot_id 중복 |
| `atom_cannot_contain` | `idea` 엔티티 Revision이 슬롯을 가짐 |
| `invalid_containment` | 부모/자식 종류 조합이 §3.3 표를 위반 |
| `schema_cross_project_composition` | Schema를 자기 출처 Project 밖의 구성에 사용 |
| `semantic_edit_required` | 숫자 토큰이 바뀐 변경을 `correction`으로 제출 |
| `correction_requires_reason` | `correction`인데 사유/대상 누락 |
| `correction_entity_mismatch` | `correction_of`가 다른 엔티티의 Revision |
| `semantic_requires_new_entity` | 기존 엔티티에 `semantic` Revision |
| `initial_already_exists` | 이미 Revision이 있는 엔티티에 `initial` |
| `composition_not_allowed_on_idea` | Idea 엔티티에 `composition` |
| `previous_revision_mismatch` | `previous_revision_id`가 다른 엔티티의 Revision |
| `missing_source_anchor` | `extracted`인데 앵커 없음 |
| `source_anchor_out_of_range` | 앵커 범위가 캡처 길이를 벗어남 |
| `source_anchor_text_mismatch` | 앵커 구간 문자열이 `body`와 다름 |
| `source_capture_mismatch` | `source.capture_id`와 앵커의 `capture_id` 불일치 |
| `content_digest_mismatch` | 제출한 `content_digest`가 실제 내용과 다름 |
| `inferred_must_not_anchor` | `inferred`인데 앵커가 있음 |
| `artifact_bytes_unsupported` | `included_in_export: true` |
| `goal_scope_mismatch` | goal의 경로가 target과 불일치 |
| `baseline_scope_mismatch` | assessment 범위가 baseline의 goal 범위와 불일치 |
| `baseline_supersede_scope_mismatch` | `supersedes` 대상 goal의 범위가 다름 |
| `official_assessment_from_proposed_goal` | AI 제안 goal의 baseline으로 공식 평가 |
| `baseline_missing_required_criterion` | baseline이 goal의 required 기준을 누락 |
| `assessment_missing_required_criterion` | 평가가 required 기준을 덮지 않음 |
| `unknown_criterion` | 존재하지 않는 criterion_id |
| `evidence_after_cutoff` | 근거의 seq/occurred_at이 cutoff보다 미래 |
| `invalid_link_endpoints` | 링크 타입의 허용 endpoint 위반 |
| `impact_evidence_mismatch` | `observed`인데 관측 없음 / `estimated`인데 관측 있음 |
| `root_change_requires_decision` | 루트 변경에 `meaningful:true`/사유/결정 누락 |
| `invalid_head_revision` | head Revision의 엔티티가 그 Project 소속이 아님 |
| `export_scope_unsupported` | 프로젝트 범위 export의 참조 폐포가 한계 초과 |
| `forbidden_kind` | `head_change`/`publication`을 `records`에 제출 |
| `slot_path_not_found` | 슬롯 경로가 해석되지 않음 |
| `embedding_dim_mismatch` | `dim != values.len()` |
| `promotion_already_exists` | 후보가 이미 승격됨 |
| `timezone_required` | `occurred_at`류에 오프셋 없음 |
| `limit_exceeded` | 문서 §2.2 한계 초과 |

---

## 7. 예제 패키지

`examples/`에 재현 가능한 패키지가 있다. LLM 키 없이 그대로 적용된다.

| 파일 | 내용 |
|---|---|
| `examples/01_software_login.json` | 소프트웨어: 6단계 깊이 로그인 스키마, 다이아몬드로 재사용되는 잠금 정책, FE/BE 역할 |
| `examples/02_login_occurrence_replace.json` | 위 구성에서 BE 경로 한 곳만 교체하는 `occurrences/replace` 요청 |
| `examples/03_modeling_hypothesis.json` | 연구/모델링: 가설 파생, 환경/데이터 버전, 실패 관측, 목표/기준선/평가 |
| `examples/04_modeling_new_baseline.json` | 기준선 변경 후 새 평가(이전 평가 유지) |
| `examples/05_qualitative_planning.json` | 비개발 기획: 정성 기준, 누락 목표, 캡처→후보→승격 |
| `examples/06_invalid_cycle.json` | 거절되어야 하는 순환 참조 패키지 |

---

## 8. 이 계약이 다루지 않는 것

3D 화면, 팀 권한/HA, 브랜치 merge, 외부 아티팩트 보관 서비스와 바이트 업로드, ANN 벡터 인덱스, 서버 측 임베딩 생성, 자율 에이전트 실행, 독립적인 하위 공식 스트림.

쓰기 CAS는 §2.6의 DB 잠금으로 다중 프로세스에서도 성립한다. 다만 **처리량**은 잠금 직렬화에 묶이며, 이 릴리스에서 측정하지 않았다.

## 시간과 조회 제한 보충

서버 기록 시각과 비교용 UTC 키는 밀리초 정밀도이며 고정 세 자리 소수부를 사용한다. 같은 밀리초의 순서는 commit seq로 구분한다. 읽기 일관성을 위해 읽기도 같은 DB 잠금을 사용하고 조회 후 롤백한다. 따라서 읽기와 쓰기가 직렬화되며, 여러 API 프로세스에서도 Neo4j 잠금을 공유한다.

## 복원 검증 보충

복원은 레코드를 commit 순서대로 다시 검증하고, 같은 commit의 기록 시각과 정규화된 데이터가 원본과 일치하는지 확인한다. head 변경의 이전 값과 현재 값, 영수증의 기록 ID·seq·head·publication도 교차 검증한다. 관계 생성 개수는 Neo4j 트랜잭션을 커밋하기 전에 검사한다.

프로젝트 단위 export는 다른 프로젝트의 기록이나 head를 참조하는 영수증을 제외하고 manifest에 `receipts_included`와 `receipts_omitted`를 기록한다. 외부 아티팩트 또는 빠진 영수증이 있으면 `complete_backup: false`다. 전체 복구에는 기본 전체 export를 사용한다. SHA-256 digest는 파일 내용의 일관성 검사이며 전자서명이나 출처 인증이 아니다. 원본 요청 본문을 보관하지 않으므로 request digest 자체의 진위는 복원 파일만으로 증명하지 못한다.

## HTTP와 용량 보충

POST는 `Content-Type: application/json`을 요구한다(문자 집합 파라미터와 +json 미디어 타입 허용). 인증 설정 시 인증부터 검사한다. CORS는 활성화하지 않는다. 전체 5,000개 레코드 한도는 서버가 생성하는 head/publication을 포함하여 쓰기 전에 강제한다. 초과 시 기존 상태를 유지하고 `validation_failed` / `limit_exceeded`로 거절한다. validate의 `derived.namespace_capacity`는 이 한도를 나타낸다. 한도에 도달해도 기존 영수증의 멱등 재생은 가능하다.
