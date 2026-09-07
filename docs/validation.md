# 검증 현황

검증일: 2026-09-07. 이 문서는 현재 MCP 구조에서 직접 확인한 결과와 과거
HTTP 쓰기 구조에서 얻은 결과를 구분한다. 아직 재실행하지 않은 항목은 통과로
간주하지 않는다.

## 최신 추가 검증: 원자화·문맥·무결성

2026-09-07 확장 결과와 재현 명령은 [context-evidence.md](context-evidence.md),
공유 가능한 실제 수치는 [요약 JSON](evidence/20260907-summary.json)에 기록했다.

- Grok/Fable의 같은 원문 12개씩을 실제 구조화하고 출력 오류·수정 이력을 보존했다.
  검토한 결과를 MCP로 저장하여 Idea 52개, 미확정 Candidate 4개, 원문/장부 Capture
  48개와 모든 Idea의 로컬 임베딩을 확인했다. 첫 응답 성공과 검토 후 적용을 구분한다.
- 39개 질문 × 어휘/하이브리드 × 3개 바이트 예산 × 5개 문맥 방식으로 근거 보존을
  측정했다. 방향 질문에 부모 설명이 도움이 되지만 256바이트에서 원자를 밀어내는
  반례가 있었다. 생성 답변 정확도나 팀 규모 성능 검증은 아니다.
- 메인 DB 474개 레코드를 읽기 전용 검사: 오류 0개. 격리 DB 최종 1,079개 레코드,
  Revision 347개, CONTAINS 478개를 전수 검사했고 강제 재시작/빈 DB 복원 후 일치했다.
- 수용 13개, MCP intake 12개, 기존 30단계 및 재시작/복원/프로세스 종료 검증을
  다시 실행했다. Rust 129개, 신규 Python 17개, 기존 JS 모형 검사도 통과했다.
- 부정 반전·비교 조건 반전·조건을 뺀 부분 인용은 구조 검사만으로 걸러지지 않았다.
  진단 validate까지만 실행했으며 잘못된 의미 변경을 적용하지 않았다.

원본 실행 결과는 로컬 `test-results/evidence-run-r5/`, 모델 응답·리뷰는
`test-results/evidence-reviews/`, 메인 무결성 증거는 `test-results/main-integrity-20260907/`
에 있다. 실행 실패와 초기 비교 결과도 함께 보존했다. 아래 섹션들은 이전 단계의
결과이며 이 최신 실행의 통과 수치와 혼동하지 않는다.

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
- 3D는 실제 Revision의 구성·버전·파생·유사/모순 연결을 표시한다. 관측·목표·평가는
  같은 대시보드의 2D 상세·검색·전체 활동 이력에서 조회한다.

## 2D·3D 대시보드 추가 검증

실제 로컬 프로젝트 `lcdb3a12d9acb6_project`에는 30개 단계와 403개 도메인 기록이
있다. 기존 사용자 기록 5개는 변경하지 않았으며 전체 408개 기록을 보존했다.
그래프는 Revision 113개, 연결 212개를 원본 export로부터 구성한다. X는 기록 시각,
Y는 최소 재귀 깊이, Z는 Schema Entity 레이어다. 공유 위치 선택과 탐색 제한을
명시하며 같은 좌표의 표시는 펼쳐도 저장된 축 값은 바꾸지 않는다.

`node tests/graph-model.cjs`는 공유 DAG·버전·연결·프로젝트 범위·시각·순환·노드 및
20,000회 탐색 제한을 확인했다. [실제 브라우저 r4](../test-results/dashboard-3d-r4-20260907/report.json)는
직접 링크, 전체 30단계, 노드·레이어 필터, 회전·확대·초기화, 실제 상세, 과거 root/seq,
역할 검색, 늦은 노드 및 프로젝트 응답의 격리, 쓰기 요청과 JS 오류 0건을 검증했다.
스크린샷도 직접 확인해 상세 빈 화면, 이력 제목, 그래프 잘림을 수정했다.

프로젝트 A 응답을 늦춘 뒤 B로 전환하고 A 응답을 풀어도 B 화면이 유지되며,
같은 프로젝트에서 먼저 클릭한 노드의 늦은 응답이 마지막 선택을 덮지 않는다.
검증 중 실패한 r2 보고서와 스크린샷을 포함한 모든 실행 로그는 보존한다.

[r5 브라우저 결과](../test-results/dashboard-3d-r5-20260907/report.json)는 위 항목과
1720px/1280px 레이아웃까지 총 11개 검사를 통과했다. 목표 목록은 별도 스크롤로
제한하여 전체 활동 이력을 가리지 않도록 했다. 메인 `8080`에 설치한 뒤 [최종 실제 UI 검사](../test-results/dashboard-final-r2-20260907/report.json)도 11개 모두 통과했다. 데스크톱·노트북 스크린샷을 직접 확인했고, JavaScript 오류와 대시보드 쓰기 요청은 0건이다.

최종 재실행 중 발견한 검증 하네스 오류도 보존했다. 실제 벡터의 Rust/Python 숫자
직렬화 차이를 HTTP 숫자 토큰 보존으로 해결했으며 digest 단언은 유지했다. macOS
Bash 3의 빈 배열 nounset 처리도 수정했다. 실행 중 스크립트 수정으로 중단된 r5
수명주기 로그는 실패 기록으로 남기고, 파일을 고정한 뒤 r6 전체 검증을 다시 실행하여 통과했다.

최종 실행 이미지는 `idea-db:dashboard-check` r3 빌드를 `idea-db:local`로 태그한 이미지다. 코드 기준은 `7b70ce7`이다. 성공·실패 호스트 실행 로그와
검증 컨테이너의 stdout/stderr 및 전체 `/logs`를 `test-results/`에 보존했다.
MCP 전체 DB·영수증 내보내기 기본 확대는 자동 승인 검토가 민감 데이터 노출
증가로 거절하여 제외했다. MCP export는 프로젝트 지정과 영수증 기본 제외를
유지하며, 기존 전체 복구용 read-only HTTP export 계약은 변경하지 않았다.


### 메인 UI에서 발견한 조회 취소 결함과 최종 회귀

첫 메인 UI 검사에서는 빠른 화면 전환 뒤 `/api/export`가 지연되어 실패했다.
실제 Neo4j에서 취소된 HTTP 요청의 트랜잭션이 전역 잠금을 보유하고, 다른 조회들이
4분 이상 대기하는 것을 확인했다. [실패 보고서](../test-results/dashboard-final-20260907/report.json)와
컨테이너·Neo4j 로그를 삭제하지 않고 보존했다.

열린 트랜잭션의 Drop은 비동기 롤백을 요청한다. begin과 commit의 HTTP 교환은 호출자
취소에도 완료되도록 보호하며, commit 결과가 불명확할 때 쓰기를 자동 재실행하지 않는다.
롤백 응답의 정확한 오류 코드를 확인하고 ConcurrentRequestAccess만 제한적으로 재시도한다.
정리 요청도 네트워크 장애나 프로세스 종료 시 실패할 수 있어 서버 타임아웃은 최종 안전장치다.

- `make check`: Rust 121개 통과, 실제 Neo4j 전용 테스트 1개는 기본 실행에서 제외.
- 제외된 테스트를 격리 Neo4j에서 별도로 실행: 잠금 보유 중 취소와 잠금 대기 중 취소 모두
  다음 조회가 5초 이내 완료되고 도메인 sequence가 유지됨을 확인.
- 최종 r7 [MCP intake](../test-results/dashboard-acceptance-r7-20260907/mcp-ingest/mcp-ingest-report.json):
  12개 통과. 같은 실행의 실DB 수용 13개와 재시작·복원·프로세스 실패·종료 검증도 통과.
- 최종 r7 [30단계 수명주기](../test-results/dashboard-lifecycle-r7-20260907/report.json):
  30단계 및 재시작·복원 후 재조회 통과.
- [설치 후 전체 기록 대조](../test-results/user-preservation-20260907/final-cancellation-fix-verification.json):
  사용자 5개와 시나리오 403개, 총 408개 기록의 전체 content와 digest 일치.

로컬 포트가 금지된 sandbox에서 처음 실행한 테스트의 PermissionDenied 실패 로그도
보존했다. 로컬 포트 사용 권한이 있는 실행에서 포맷·린트·전체 테스트를 다시 통과했다.


### 최신 요약·버전 diff·Schema 목표 보완 (2026-09-07)

실행 코드 `6e54e1d`를 포함한 `idea-db:dashboard-summary-check` R2 이미지를 메인
`idea-db:local`에 반영했다. 이름과 기록 시각을 기본 표시하고 ID·해시·원시 JSON은
접힌 기술 정보로 이동했다. 프로젝트 첫 화면에는 여섯 Schema 요약이 나오며,
좌측 탐색기와 요약 카드를 누르면 중앙 상세·버전 비교와 해당 Schema 목표가 갱신된다.
데스크톱 각 영역은 독립 스크롤을 사용하며 2D/3D 전환은 유지한다.

버전 비교는 이전 Revision 또는 명시적 파생 관계를 따른다. 본문 추가/삭제를
강조하고 연결·역할 변경, 메타데이터 변경과 저장된 변경 이유를 표시한다.
이전 후보가 여럿이면 비교 대상을 선택하며, 이름의 v2/v3만으로 계보를 추정하지 않는다.

목표 조회는 Schema 자체 목표와 하위 목표, 공식 평가와 AI 제안을 분리한다.
정량 목표 기준과 실제 관측/AI 추정값을 함께 표시하고 값이 없으면 미측정으로 남긴다.
과거 root의 목표는 현재 충족도에 합산하지 않는다. 새 미평가 기준선이 이전 충족
상태를 이어받던 결함도 수정했다.

현재 데모의 기존 목표는 과거 v2 Idea 목표 18개와 현재 v3 BE 하위 Idea 목표 1개였다.
현재 Schema 자체 목표 여섯 개는 비어 있었다. 로컬 AI가 여섯 목표와 정량 기준
18개를 작성하고, 출처 캡처 → MCP 검증 → 미리보기 검토 → 적용으로 저장했다.
모두 `ai_proposed`이고 수치 임계값은 검토가 필요한 초기 제안이다. 실제 관측값과
완료 확률을 만들지 않았으며 평가 상태는 unknown이다. 기존 BE 하위 목표의 공식
정량 기준 1개와 합쳐 최신 화면에는 정량 기준 19개가 표시된다.

- `make check`: Rust 124개 통과(라이브러리 122 + 실행 파일 2), 포맷·린트 통과.
  실제 Neo4j 취소 회귀 1개는 기본 실행에서 제외하며 앞 절의 별도 실DB 결과를 유지한다.
- `node tests/dashboard-model.cjs`, `node tests/graph-model.cjs`: 통과.
- [메인 요약 UI](../test-results/dashboard-summary-final-r1-20260907/report.json): 8개 통과.
  해시 기본 숨김, 본문/연결 diff, 연속 Schema 선택, 최신 복귀, 여섯 목표·18개 AI 기준,
  과거 실측 3과 기준 2의 비교 및 최신 미측정 분리, 1720/1280px 화면을 확인했다.
- [메인 탐색 UI](../test-results/dashboard-navigation-final-r4-20260907/report.json): 11개 통과.
  원래 30단계 보존, 113개 Revision 그래프, 회전·확대·초기화·필터·선택, 이력·검색,
  늦은 노드/프로젝트 응답 격리를 확인했다. 두 UI 실행 모두 JS 오류와 쓰기 요청 0건이다.
- 새 이미지의 `scripts/docker-check.sh`: 실DB 수용 13개, MCP intake 12개와 강제 종료 후
  재시작·새 볼륨 복원·필수 프로세스 실패 전파·정상 종료를 통과했다.
  [실행 로그](../test-results/logs/dashboard-summary-20260907/idea-db-summary-acceptance-20260907.log)와
  `test-results/dashboard-summary-acceptance-20260907/`의 MCP·컨테이너·Neo4j 로그를 보존했다.
- [데이터 대조](../test-results/dashboard-summary-install-20260907/preservation.json):
  반영 전후 433개 기록의 전체 content와 digest 일치. 원래 408개 기록은 그대로이고,
  출처 캡처 1개와 검토한 목표 패키지 24개만 추가됐다. 원래 30단계에 감사 이력 2단계가
  추가되어 현재 활동은 32단계다. 목표 제안 전후 원본 및 head 보존 대조는
  `test-results/dashboard-goal-proposals-20260907/preservation.json`에 있다.

실패 증거도 유지했다. 미리보기 r1은 기존 BE 기준을 빠뜨린 18개 총수 단언,
r2는 상세 비동기 응답을 기다리지 않은 단언으로 실패했고 r3은 8개 통과했다.
메인 탐색 r1은 다른 격리 프로젝트 보고서를 지정해 실패했다. r2/r3은 접힌 기술
정보에 `innerText`를 사용한 검증 오류였으며, 실제 선택 응답과 `textContent`를
검증하는 r4에서 11개를 통과했다. 초기 전체 export 비교는 내보내기 시각인
`exported_at` 차이로 실패했고, 도메인 content 및 digest 비교로 보존을 확인했다.
성공·실패 실행 로그, 스크린샷, 미리보기 컨테이너 stdout/stderr와 전체 `/logs`를
삭제하지 않았다. 임시 미리보기만 정지했고 데이터·로그 볼륨은 유지한다.

이 검증은 저장된 목표·관측·버전의 올바른 표시를 증명한다. 새 제안의 기준이 사용자
프로젝트에 적합하다는 검토나 최신 목표의 실측 달성을 증명하지는 않는다. 기존 Fable/Grok
검수는 이전 핵심 구현에 대한 것이며 이번 화면은 Sol/Terra 작업을 Main이 검토하고
실제 메인 브라우저에서 수용 검증했다.


### Project·Schema·Core 예상 달성률과 전체 버전 계보 (2026-09-07)

이전 구현은 목표 임계값과 미측정 상태만 보여 AI 예상 달성률 요구를 충족하지 못했다.
이번에는 `CriterionResult.progress_estimate`에 AI가 작성한 점수·이유·근거 ID를 저장한다.
Project, Schema, 재귀 Core 각각의 현재 root/사용 경로에 자체 목표와 평가를 둔다.
현재 기준선의 필수 기준 전부가 평가되었을 때만 그 목표 안에서 같은 비중으로 평균한다.
자식 개수나 자식 점수를 부모 달성률로 자동 평균하지 않는다. 공식 실측과 gate는 별도다.

메인 데모는 기존 Schema 목표 6개에 새 평가를 추가하고 Project 목표 1개와 Core 목표
6개를 추가했다. 총 13개 목표의 39개 기준을 현재 기획·원자 Idea·기존 관측과 대조했다.
대부분은 기획 반영 단계인 20%다. BE의 상태 전이 Idea에는 이전 장애 대응 문맥이 남아
해당 기준을 0%로 평가했고, BE Schema/Core 목표는 각각 약 13%다. 퍼센트는 문서화한
AI 이정표 추정이며 실제 성능이나 성공 확률이 아니다. 모든 새 관측값은 null이고 실제
검증 상태는 unknown이다. 원문 캡처·MCP 검증·미리보기 검토·적용 증거는
`test-results/hierarchical-progress-20260907/`에 있다.

버전 비교는 선택 항목의 명시적 이전/파생 계보를 양방향으로 찾아 시작과 끝 버전을
고를 수 있다. v1→v2, v1→v3 및 원자 Idea의 이전 버전 비교를 실제 화면에서 확인했다.
같은 commit의 명시적 계보도 보존하며 제목만으로 계보를 만들지 않는다. 각 버전의
변경 이유·원문·관측·목표·평가를 `이 항목의 방향과 기록`에서 볼 수 있다. 서버 기록
cutoff와 관측/평가의 발생·근거 시점 필터를 적용한다.

- `make check`: Rust 129개(라이브러리 127 + 실행 파일 2), 포맷·린트 통과.
  실DB 취소 회귀 1개는 기본 제외하며 이전 별도 검증 결과를 유지한다.
- `dashboard-model`, `version-history`, `graph-model` Node 검사 통과.
- 최종 R5 이미지 메인 UI: [계층별 목표/버전 검사 9개](../test-results/dashboard-progress-r5-final-20260907/report.json),
  [요약/diff 검사 8개](../test-results/dashboard-summary-r5-final-20260907/report.json),
  [3D/탐색 검사 11개](../test-results/dashboard-navigation-r5-final-20260907/report.json) 통과.
  13개 목표·39개 추정값·근거 링크·과거 공식 실측·최신 복귀·1720/1280px 화면을 확인했다.
  모든 UI 실행의 JavaScript 오류와 대시보드 쓰기 요청은 0건이다.
- 새 backend 이미지의 독립 Docker 검증: 실DB 13개, MCP intake 12개, 강제 종료 후
  재시작·빈 볼륨 복원·필수 프로세스 실패 전파·정상 종료 통과. R3~R5는 정적 UI 수정이며
  backend는 같은 코드다. 기록은 `test-results/progress-acceptance-20260907/`에 있다.
- [새 추정의 MCP 복원](../test-results/progress-recovery-r2-20260907/report.json):
  격리 Neo4j에 469개 프로젝트 기록, head, 영수증, sequence를 동일하게 복원했고
  39개 추정과 명시적 0 두 개를 재조회했다. import가 원본 digest를 검증했다.
  다른 프로젝트가 없는 격리 DB는 manifest의 `receipts_omitted`가 1→0으로 달라져
  재-export 전체 digest는 달라진다. 도메인 기록·영수증은 동일하며 이를 분리해 검증했다.
- 메인 전체 474개 기록 중 원래 433개와 head는 그대로이고, 출처 캡처 1개와 패키지
  40개만 추가되었다. 원래 30단계는 유지되며 제안 작성의 감사 이력 4단계를 더해
  현재 프로젝트 활동은 34단계다.

검증 중 발견한 결함과 실패 기록도 유지한다. 첫 UI 검사에서는 이력 로딩 도중 누른
Idea 선택을 자동 루트 선택이 덮었다. 초기 로딩의 record generation을 검사하고
중복 루트 선택을 제거했으며, goals 응답을 지연시킨 실제 브라우저 회귀가 통과했다.
전체 계보의 역방향 탐색 누락·발생 시점 필터 누락도 수정했다. 원문 제목에 JSON
내부 해시가 드러나는 회귀는 기존 구조화 원문 제목 helper를 재사용해 해결했다.
복원 r1/r2의 최초 전체 digest 단언은 제외된 영수증 수 메타데이터 차이로 실패했다.
실패 export·스크린샷·실행 로그와 수정 후 대조 결과를 모두 남겼다.

기존 데이터·Neo4j 로그 볼륨, 성공/실패 검증 자료와 복원 컨테이너·볼륨은 보존한다.
중간 R2→R3 메인 컨테이너 교체에서는 종료 전 stdout 별도 복사를 누락했다.
그 구간의 Neo4j `/logs` 볼륨과 MCP/검증 로그는 남아 있지만 해당 컨테이너 stdout
전체가 별도 파일로 보존되었다고 주장하지 않는다. 이후 교체 전후 stdout와 전체
Neo4j 로그는 `test-results/progress-install-20260907/`에 별도로 복사했다.
# 2026-09-07 원자 요약·검색 문맥 개정

- `make check`: Rust 133개 통과(라이브 DB 취소 테스트 1개는 기본 실행에서 제외). `tests/evidence_test.py` + `tests/integrity_audit_test.py`: Python 18개 통과. 대시보드 모델/버전/그래프 JS 검사 통과.
- `IDEA_DB_SKIP_BUILD=1 IDEA_DB_TEST_IMAGE=idea-db:summary-v2 IDEA_DB_SUITE=evidence IDEA_DB_REPORT_DIR=.../summary-evidence-r2 bash scripts/docker-check.sh`: 최종 독립 이미지에서 MCP 수용 13개, 기존 intake 12개, lifecycle 30단계, 문맥 질문 39개, 실제 기록 모델 출력 24사례, 의미 반례 10개, 새 요약 출처 오류 10종과 공유 경로/시점/검색 비교 통과.
- 전체 Neo4j 1,124개 기록·362개 Revision·494개 CONTAINS 관계 무결성 확인. abrupt kill/restart와 빈 DB restore 이후 export 내용/digest 동일, 과거 30단계/3개 비교 root 재조회, 필수 프로세스 사망과 정상 종료 검증 통과.
- 브라우저 `tests/summary-ui.cjs`: 실제 복원 DB에서 AI 요약/본문 분리, 요약 없는 기존 기록 전환, 2D/3D 캔버스, 쓰기 요청/브라우저 오류 없음 확인. 스크린샷은 `test-results/summary-ui-r1/`에 보존.
- 메인에 최종 이미지를 적용한 뒤 474개 기록·114개 Revision·2개 head·33개 receipt와 export digest가 업데이트 전과 정확히 같았다. 실제 MCP의 기존 body-v1 검색도 통과했다. `test-results/main-ui-after-summary-13c9b8b/`에서 기존 13개 자체 목표·39개 기준, 버전 범위 비교 등 UI 9항목을 확인했고 `summary-ui-final-13c9b8b/`에서 최종 이미지 요약 UI 4항목을 재확인했다. 메인 중지 전후 stdout/stderr와 Neo4j `/logs`는 `main-summary-deploy-13c9b8b/`에 보존했다.
- [설계와 측정 한계](atomization-and-summary.md), [최종 이미지·수치](evidence/20260907-summary-embeddings.json). 사람이 작성한 합성 요약 6쌍/6질의이며 생성 모델 품질이나 일반 성능 검증이 아니다. 잘못된 의미의 요약을 자동 거절하는 의미 검증기는 구현하지 않았다.
- 모든 성공/실패 로그와 두 차례 전체 실행 결과를 별도 이름으로 보존했다. 초기 단위 테스트의 source_kind fixture 오타는 수정 후 통과했고, 기존 사용자 기록을 테스트 기대값에 맞춰 고치지 않았다.
