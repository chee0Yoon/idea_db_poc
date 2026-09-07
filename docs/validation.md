# 검증 현황

검증일: 2026-09-07. 이 문서는 현재 MCP 구조에서 직접 확인한 결과와 과거
HTTP 쓰기 구조에서 얻은 결과를 구분한다. 아직 재실행하지 않은 항목은 통과로
간주하지 않는다.

## 현재 구조와 검증 환경

- 로컬 AI 클라이언트가 원문을 원자화하고 Project → Schema → 재귀 Core →
  Idea 패키지와 출처 coverage ledger를 만든다. 서버는 의미 분해를 대신하지
  않고 구조·그래프·출처·CAS를 검증한다.
- 모든 생성·변경·복원은 stdio MCP를 거친다. HTTP API와 대시보드는 읽기
  전용이며 브라우저에서 쓰기 요청을 보내지 않는다.
- MCP preview는 로컬 Ollama embedding을 생성하고 준비 패킷을 Neo4j에
  저장한다. apply는 검토한 `upload_id`와 `prepared_digest`만 받아 모델 호출
  없이 원자적으로 커밋한다.
- 실행 환경은 macOS ARM64, Rust 1.98.0, Neo4j 5.26.30 Community와 Docker
  Desktop Engine 28.5.1이다. Rust API와 Neo4j는 digest로 고정한 standalone
  이미지 하나에서 실행했고, 호스트 Python/Node는 검증 클라이언트로만 썼다.

## 현재 확인된 결과

| 검증 | 결과 | 근거 |
|---|---|---|
| 최종 `make check` | 통과: fmt, Clippy `-D warnings`, library 115개, MCP binary 2개 (총 117개) | [정적 검사 로그](../test-results/logs/host-runs-20260907/idea-db-mcp-check-r7.log) |
| MCP intake | 12개 항목 통과, 실제 Ollama `embeddinggemma:300m` 사용 | [mcp-ingest-report.json](../test-results/mcp-acceptance-r6-20260907/mcp-ingest/mcp-ingest-report.json) |
| 읽기 전용 UI | 7개 항목 통과, 쓰기 요청 0건, 브라우저 예외 0건 | [UI 보고서](../test-results/ui-mcp-r4-20260907/report.json), [검색 화면](../test-results/ui-mcp-r4-20260907/capture-search.png), [Idea 상세](../test-results/ui-mcp-r4-20260907/idea-detail.png) |
| MCP 30단계 수명주기 r6 | 30/30, 강제 재시작·빈 볼륨 복원·API 프로세스 상실·정상 종료 검증 통과 | [단계 보고서](../test-results/mcp-lifecycle-r6-20260907/report.md), [배포 검사](../test-results/mcp-lifecycle-r6-20260907/deployment-checks.json) |
| MCP 수용 r6 | 13/13 + intake 12/12, 재시작·복원·프로세스 상실·정상 종료 모두 통과 | [전체 로그](../test-results/logs/host-runs-20260907/idea-db-mcp-acceptance-r6.log) |

최종 정적 검사 로그는 library 테스트 115개와 `idea-db-mcp` binary 테스트 2개가
모두 통과했음을 기록한다. 이 수치는 네트워크·Docker 수용 결과를 대신하지
않는다.

MCP intake 검사는 원문 Capture 저장, 준비 패킷의 벡터 비노출, recursive
패키지 적용, Idea embedding과 Unicode source span 보존, 잘못된 출처의 원자적
거절, 숫자·부정 변경의 명시적 검토, digest/handle 우회 차단, handle 재생,
MCP hybrid 검색, 검토한 similar 링크, embedding provider 실패 시 write 없음까지
확인했다. 사용한 profile은
`embeddinggemma:300m@sha256:85462619ee721b466c5927d109d4cb765861907d5417b9109caebc4e614679f1/idea-body-v1`이다.

이 intake의 Project/Schema/Core/Idea 구조는 검증 클라이언트가 공급한 결정적
fixture다. 로컬 AI가 기획서를 자율적으로 해석해 원자화했다는 증거가 아니다.
실제 Ollama 실행은 embedding 생성과 hybrid 검색 경로를 검증한다.

## MCP 30단계 수명주기 r6

r6는 고객지원 분류에서 운영 장애 분석, 사내 지식 검토로 방향을 바꾸는 한
Project를 MCP 경유로 30회 순차 커밋했다. FE/BE/Infra/ML/DL/LLM 역할, 재귀
구성, 공유 Idea, 부분 변경과 파생 계보, 목표·기준선·관측·평가, 발생 시각과
기록 시각을 함께 검증했다.

| 항목 | 실제 결과 |
|---|---|
| 데이터 | seq 30, domain record 403개: Revision 113, Entity 92, Embedding 56, Capture 30 등 |
| 실행 근거 | 작은 기술 workload 18개와 늦게 도착한 관측 1개, Artifact 19개 |
| 고정 rubric | workload 18개 중 충족 16개, 미충족 2개; 늦은 관측 평가 1개 별도 |
| 검색 | 자연어 한국어 질의 6개에서 기대 ID Recall@5=1.0, 역할 제외와 과거 Revision 확인 |
| 계보·출처 | 부분 파생 6개, Link 4개, Capture 출처 9개 재조회 |
| 시간 | 30개 `known_seq`/`known_at`, publication 3개의 직전·정각·직후와 timezone 동치 확인 |
| 복구 | 강제 재시작과 빈 볼륨 import 후 content/digest 동일 및 읽기 전용 재검증 통과 |

전체 export digest는
`sha256:4ded30836b4af4c44d606d2a0b61aa616e399017a846353c21a9de3cb975e79b`이다.
[기계 판독 보고서](../test-results/mcp-lifecycle-r6-20260907/report.json),
[checkpoint](../test-results/mcp-lifecycle-r6-20260907/checkpoint.json),
[전체 export](../test-results/mcp-lifecycle-r6-20260907/export.json)를 함께 보존한다.

18개 workload는 실제로 실행한 작은 프로토타입이다. ML/DL은 합성 데이터의
소규모 학습·평가이고 LLM 역할은 실제 생성 모델 추론이 아닌 결정적 adapter
계약 fixture다. 이 결과로 상위 제품 기능의 완료나 팀 규모 성능을 주장하지
않는다. Recall@5 역시 고정 질의의 기능 회귀이며 일반적인 의미 검색 품질
평가가 아니다.

## 외부 리뷰와 조치

Grok 4.6과 Claude Fable 5.1 리뷰 원문은
[`test-results/reviews/`](../test-results/reviews/)에 그대로 보존한다. 리뷰는
코드 판독 결과이며 자체 실행 증명은 아니다. Main이 재현 가능성과 현재 코드를
대조해 다음처럼 처리했다.

- `query_vector`/`vector` 불일치, MCP apply 문서 불일치, 클라이언트 embedding·
  검색 vector 우회, 새 Idea occurrence의 indexing 우회는 수정했다.
- 준비 패킷은 pending 256개만 용량에 포함한다. 적용 이력은 삭제하지 않으며,
  domain receipt와 `applied_seq`를 같은 Neo4j transaction에서 기록한다. 복원은
  기존 handle을 삭제하지 않고 무효화한다.
- embedding profile은 preview 패킷에 고정되고 apply 재시도는 모델을 호출하지
  않는다. provider 오류와 profile 변경은 조용한 lexical fallback으로 처리하지
  않는다.
- 잘못된 head·빈 패키지·`validation.valid=false`를 embedding 전에 거절하는
  경로와 Idea 전용 index coverage 필드는 현재 코드에 반영됐다. 이 변경까지
  포함한 r6 Docker 수용 13개와 intake 12개가 통과했다.
- 버려진 preview는 MCP `idea_upload_discard`로 철회하며 문서와 시각을 보존한다.
  철회·복원 무효화·적용은 같은 저장 잠금에서 확인하고 marker 1건 갱신을 검증한다.
- 대형 preview의 staging 크기 추정과 process interruption 경계에 관한 리뷰는
  보수적 사전 한도와 원자적 applied marker로 보강했다. 실제 2 MiB 경계의
  대규모 Ollama 호출 비용·지연은 별도 부하 시험으로 검증하지 않았다.

r4 MCP 수용의 동시 수정 단계에서는 Neo4j transient deadlock이 한 차례
발생했다. 전체 operation을 새 transaction에서 제한적으로 재시도하도록 수정했고
retry 단위 회귀와 최종 r6 동시 수정 검증이 모두 통과했다.

## 과거 결과

이전 문서의 `idea-db:acceptance` 이미지, Rust 테스트 88개, 실DB 수용 12개와
347-record lifecycle 결과는 MCP 쓰기 경계 도입 전 버전에서 측정한 역사적
baseline이다. 당시 6단계 diamond, 동시 same-head 한 승자, 시점 조회,
publication 유효 시각, 역할 검색, 후보 승격, 복원·종료를 검증하는 데 유효했지만
현재 빌드의 통과 수치로 사용하지 않는다. 현재 근거는 위 최종 정적 검사, r6 MCP
lifecycle, MCP intake와 UI 결과다.

## 보장하지 않는 범위

- DB는 원자화의 의미적 완전성이나 AI 판단의 진실을 증명하지 않는다. source
  span과 coverage ledger도 사람이 원문과 대조해야 한다.
- 검색은 제한된 그래프의 한국어 lexical 점수와 클라이언트가 선택한 로컬
  embedding profile의 exact cosine/RRF를 쓴다. ANN, 학습형 ranking, 실제 업무
  corpus의 precision/recall은 검증하지 않았다.
- 전체 namespace는 5,000 domain record로 제한한다. 팀 부하, HA, p95 latency,
  인터넷 공개 운영과 권한 분리는 이번 검증 범위가 아니다.
- Artifact는 URI와 digest를 저장한다. digest는 일관성 검사이며 출처 인증이나
  외부 바이트의 영구 보관을 뜻하지 않는다.
- 현재 UI는 2D 읽기 전용 탐색·검색·이력·근거 확인 범위다. 3D 화면은 검증하지
  않았다.

최종 재실행 중 발견한 검증 하네스 오류도 보존했다. 실제 벡터의 Rust/Python 숫자
직렬화 차이를 HTTP 숫자 토큰 보존으로 해결했으며 digest 단언은 유지했다. macOS
Bash 3의 빈 배열 nounset 처리도 수정했다. 실행 중 스크립트 수정으로 중단된 r5
수명주기 로그는 실패 기록으로 남기고, 파일을 고정한 뒤 r6 전체 검증을 다시 실행하여 통과했다.

최종 이미지는 `idea-db:mcp-check`의 r6 빌드다. 성공·실패 호스트 실행 로그와
검증 컨테이너의 stdout/stderr 및 전체 `/logs`를 `test-results/`에 보존했다.
MCP 전체 DB·영수증 내보내기 기본 확대는 자동 승인 검토가 민감 데이터 노출
증가로 거절하여 제외했다. MCP export는 프로젝트 지정과 영수증 기본 제외를
유지하며, 기존 전체 복구용 read-only HTTP export 계약은 변경하지 않았다.
