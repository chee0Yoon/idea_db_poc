# 작업 하네스

Main이 요구사항, API 계약, 파일 소유권, 의존 관계와 통합 수용을 관리한다. 외부 모델의 답변이나 자체 테스트 통과만으로 완료 처리하지 않는다.

## 작업 순서

1. `plan.md`와 현재 `docs/validation.md`를 읽고 해결할 요구사항/불변 조건을 지정한다.
2. 공유 API가 바뀌면 `docs/api.md`를 먼저 수정하고 호출자와 fixture에 미치는 영향을 확인한다.
3. 변경 범위가 독립적일 때만 작업자를 나눈다. 한 소스/스키마를 두 작업자가 동시에 수정하지 않는다.
4. 작업자는 소유 파일과 테스트 결과를 Main에 반환한다. Main이 실제 diff를 검토한다.
5. `make check` → 실행 중인 전용 DB에 `make acceptance BASE_URL=...` → `make docker-check` 순서로 검증한다. 마지막 검증은 자체 임시 컨테이너/볼륨만 사용한다.
6. 발견 결함을 수정하고 영향을 받는 검증을 재실행한다. 남은 한계와 실제 측정 조건을 기록한다.

## 이번 구현의 작업 분담

| 작업 | 담당 | 소유 파일 | 전달물 |
|---|---|---|---|
| 설계·배포·통합·문서 | Main | plan, AGENTS, Docker/Compose, scripts, README, CI | 실행 이미지, 검증 보고서 |
| 저장/API/복구 | Opus 5 / ACP | Cargo, src/store·mutation·backup·http·neo4j, examples | 계약, 구현, 단위 검증 |
| 도메인 불변 조건 | Main | src/model·validate·graph·util·limits | 모델 통합과 회귀 검증 |
| 조회/2D GUI | Terra / native | src/query.rs, static | 실제 조회 투영과 화면 |
| 독립 수용 시나리오 | Sol / native | tests/acceptance.py | 실DB 시나리오, 논리 결함 목록 |

작업자는 별도 모델 서버나 상시 백그라운드 자동화를 설치하지 않는다. 이 저장소의 재현 가능한 하네스는 문서 계약·검증 명령·CI로 구성한다. 실행 시 외부 모델 비용이 자동 발생하지 않는다. 로컬 AI 입력 방법은 `skills/idea-db-input/SKILL.md`에 정의한다.

## 변경별 수용 기준

- 저장·버전·평가·트랜잭션 변경: 도메인 테스트와 실DB 회귀 시나리오.
- 화면/API 연결 변경: 해당 입력/조회 흐름 실제 브라우저 확인, 오류 상태와 HTML escaping 확인.
- Docker/복구 변경: 신선한 이미지 빌드, readiness, 종료, 재시작, export/import.
- 문구·스타일 등 낮은 영향 변경: 해당 파일/화면 확인. 불필요한 전체 재검증은 하지 않는다.

인증 정보는 `.env`와 클라이언트 환경에 보관하며 커밋하지 않는다. 사용자의 선택에 따라 이번 작업은 로컬 Git만 사용하고 원격 생성·push를 하지 않는다.
