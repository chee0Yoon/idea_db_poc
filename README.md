# idea_db

Rust + Neo4j로 만든 버전형 아이디어 DB. Project → Schema → Core(재귀) → Idea를 구성하고, 공유 아이디어의 사용 위치·목표·기준선·관측·평가·방향 변경 이력을 보존합니다.

기획은 [plan.md](plan.md), 데이터/요청 계약은 [docs/api.md](docs/api.md), 작업과 검증 방식은 [docs/harness.md](docs/harness.md)를 참고하세요. 이 저장소는 기존 Python 실험판과 별개입니다.

## Docker 빠른 시작

Docker Desktop 또는 Docker Engine + Compose v2, `make`, `openssl`이 필요합니다. 호스트에 Rust나 Python DB 런타임을 설치할 필요는 없습니다.

```sh
make up
```

처음 실행할 때 임의의 Neo4j 암호를 `.env`에 만들고 이미지를 빌드합니다. 준비되면 브라우저에서 <http://127.0.0.1:8080>을 엽니다. 빈 DB로 시작하며 테스트 자료가 자동으로 들어가지는 않습니다.

```sh
make logs                 # API + Neo4j 로그
make down                 # 컨테이너 종료, 데이터 볼륨 보존
```

기본 이미지는 **하나의 컨테이너 안에서 Neo4j와 Rust API를 실행**합니다. 데이터와 로그는 각각 `/data`, `/logs` 볼륨에 저장합니다. Neo4j 포트는 컨테이너 내부 loopback에만 바인딩되고, 외부에는 웹/API 포트만 호스트 loopback으로 노출합니다. 둘 중 필수 프로세스가 종료되면 컨테이너가 실패 상태로 종료됩니다.

Compose 없이도 실행할 수 있습니다.

```sh
docker build -t idea-db:local .
docker run -d --name idea-db \
  -p 127.0.0.1:8080:8080 \
  -e NEO4J_PASSWORD="$(openssl rand -hex 24)" \
  -v idea-db-data:/data -v idea-db-logs:/logs idea-db:local
```

같은 데이터 볼륨을 재사용할 때는 **초기에 사용한 DB 암호도 보관해 재사용**해야 합니다. 실행 중인 DB 암호를 환경 변수만 바꿔 갱신할 수는 없습니다. 반복 실행에는 `.env`를 보존하는 Compose 방식을 권장합니다.

## 범위

한 명의 신뢰된 소유자가 여러 로컬 클라이언트로 쓰는 MVP입니다. 선택적 API bearer token을 지원하며 팀 권한, 인터넷 서비스 운영, HA는 후속 범위입니다. 대시보드는 조회 전용입니다. 원자화와 구조화는 로컬 구독 AI 클라이언트가 담당하고 모든 변경은 MCP로 제출합니다. MCP는 로컬 Ollama 임베딩을 생성하지만 생성형 LLM을 실행하지 않습니다.

본문·관측·평가는 불변 기록입니다. 의미가 바뀌는 Idea는 파생으로 생성하고, 조합은 정확한 Revision을 고정합니다. 검색의 역할은 occurrence에 붙으며, 엄격 일치와 관련 문맥을 구분합니다. 대시보드의 어휘 검색은 모델 없이 동작합니다. MCP 하이브리드 검색과 Idea 업로드는 로컬 임베딩 설정이 필요하며 실패 시 명시적 오류를 반환합니다.

## 개발 및 검증

Rust 1.98, 실제 Neo4j 5.26.30 Community로 개발합니다. Neo4j 저장소에 연결하는 환경 변수는 `NEO4J_URI`, `NEO4J_USER`, `NEO4J_PASSWORD`이고, API 바인딩은 `IDEA_DB_BIND`, 정적 파일은 `STATIC_DIR`입니다.

```sh
make check
make acceptance BASE_URL=http://127.0.0.1:8080
make docker-check
make lifecycle-check     # 30단계 기획 전환·역할별 실행·시간 조회·복구
```

acceptance는 실행 중인 DB에 고유 ID를 가진 테스트 Project를 추가합니다. 사용자 데이터 초기화/삭제는 하지 않습니다. 독립적인 검증 DB에는 `make docker-check`를 사용하세요. Python 3의 표준 라이브러리는 **검증/클라이언트 도구**에만 사용하며 이미지 DB 런타임에는 필요하지 않습니다.

30단계 검증의 입력 기획서는 [examples/lifecycle_30](examples/lifecycle_30/), 실행 범위와 단계 구성은 [docs/lifecycle.md](docs/lifecycle.md)에 있습니다. `make lifecycle-check`는 별도 임시 DB를 만들고 검증 후 정리하며, 결과와 실행 산출물은 `test-results/`에 보존합니다.

API만 별도로 배포하려면 `docker build --target api -t idea-db-api:local .`로 빌드하고 idea_db 전용 Neo4j 데이터베이스의 `NEO4J_URI`와 인증을 설정합니다. 기본 standalone 이미지와 API-only 이미지는 동일한 Rust 코드를 사용합니다.

## 구현 선택과 한계

Neo4j의 개별 노드와 타입 관계가 권위 저장소이며 응용 ID를 사용합니다. 5.26 LTS에 고정된 트랜잭션 API를 사용합니다. 해당 HTTP API는 [5.26에서 deprecated](https://neo4j.com/docs/http-api/current/transactions/)되었으므로 Neo4j 메이저 업그레이드 전에 드라이버 변경과 수용 검증이 필요합니다.

초기 정확성 검증을 위해 읽기와 쓰기를 직렬화하고 전체 네임스페이스를 5,000개 레코드로 제한합니다. 소규모 검증 결과를 팀 규모 성능 보장으로 해석하지 않습니다. 상세 구현 한계와 실행한 검증은 [docs/validation.md](docs/validation.md)에 기록합니다.

Neo4j Community는 공식 [Docker 이미지](https://hub.docker.com/_/neo4j/)를 기반으로 하며, 해당 구성 요소의 라이선스와 공지는 원본 이미지/배포물에 포함됩니다. 응용 DB 엔진 자체를 Rust로 다시 구현하는 작업은 이번 범위에 포함하지 않습니다.

## 로컬 AI와 MCP 연결

[입력 계약](docs/mcp-intake.md)과 [로컬 AI 스킬](skills/idea-db-input/SKILL.md)을 사용합니다.
대시보드에는 생성·수정·삭제·업로드 기능이 없으며, 기존 REST 쓰기 요청도 405로 거절합니다.

1. 호스트의 Ollama에서 `ollama pull embeddinggemma:300m`을 실행합니다(약 622MB).
2. `make up`으로 Neo4j·대시보드를 시작합니다. Compose는 호스트 Ollama를 사용합니다.
3. 로컬 AI 클라이언트의 MCP 설정에 다음 stdio 서버를 등록합니다. 명령 경로를 이 저장소의 절대 경로로 바꿉니다. 예시는 `mcp.example.json`에도 있습니다.

```json
{
  "mcpServers": {
    "idea_db": {
      "command": "/absolute/path/to/idea_db_neo4j/scripts/idea-db-mcp",
      "args": []
    }
  }
}
```

실행기를 `cwd`로 지정할 수 없는 클라이언트는 `docker compose -f /absolute/path/compose.yaml exec -T idea-db idea-db-mcp`에 해당하는 인수 배열을 사용합니다. MCP는 공식 Rust SDK rmcp 3.2.0으로 구현하며 stdio 전송을 사용합니다. 생성형 AI 구독과 임베딩 API는 별개이므로, 임베딩은 지정한 로컬 모델을 사용합니다. DB 이미지 안에 생성형 모델이나 구독 토큰을 넣지 않습니다.

흐름은 **원문 보존 → 로컬 AI 원자화·구조화 → MCP preview → 임베딩·유사 후보 검토 → 업로드 ID로 apply**입니다. 유사 후보가 자동 병합되지는 않습니다. 삭제는 새 조합에서 슬롯을 제거하는 변경이며 과거 기록은 남습니다.

CLI도 동일한 MCP를 사용합니다. `apply`에는 원본 패키지가 아니라 preview의 `upload_id`와 `prepared_digest` 두 필드만 넣습니다.

```sh
export IDEA_DB_MCP_COMMAND_JSON='["docker","compose","exec","-T","idea-db","idea-db-mcp"]'
python3 scripts/idea-db-client.py validate package.json
python3 scripts/idea-db-client.py preview package.json > preview.json
# preview를 검토하고 두 필드로 apply.json을 작성합니다.
python3 scripts/idea-db-client.py apply apply.json
python3 scripts/idea-db-client.py export --output backup.json
# 복구 대상의 MCP 명령을 지정한 뒤 빈 DB에만 복원합니다.
python3 scripts/idea-db-client.py import backup.json
```

`IDEA_DB_EMBEDDING_URL`은 허용된 로컬 Ollama origin만 받으며, `IDEA_DB_EMBEDDING_MODEL`은 설치된 모델 이름입니다. 모델 manifest digest와 입력 형식을 벡터 프로필에 고정합니다. 같은 이름의 모델이 바뀌면 과거 벡터를 같은 공간으로 비교하지 않습니다. 미설정/모델 없음/너무 긴 입력/잘못된 벡터는 저장 전에 거절합니다.

스테이징은 미적용 대기 상태 최대 256건, 건당 2MiB까지 보존합니다. 적용 완료 패킷과 로그는 삭제하지 않으며 대기 한도에서 제외합니다. 버린 초안은 `idea_upload_discard {upload_id}`로 철회하면 이력을 남기고 대기 한도를 반환합니다. 컨테이너 재시작 후에도 업로드 ID를 쓸 수 있습니다. domain export에는 포함되지 않으므로 복원 후에는 새 preview가 필요합니다.

기존 30단계 자료는 구조화된 클라이언트 fixture이며 생성형 AI의 원자화 품질을 증명하지 않습니다. 새 MCP 검증은 실제 로컬 임베딩, 후보 매핑, 저장, 재시도 및 쓰기 우회 차단을 별도로 확인합니다.

## 2D·3D 대시보드

프로젝트를 선택하고 탐색기의 `2D`/`3D` 버튼으로 전환합니다. `/?project=<project_id>&view=3d`로 특정 프로젝트를 직접 엽니다. X는 서버 기록 시각, Y는 최소 재귀 깊이, Z는 스키마 레이어입니다. 드래그/화살표로 회전하고 휠/+/-로 확대합니다. 스키마 필터, 실제 노드 목록, 공유 Revision 사용 위치 선택을 제공합니다.

전체 이력 패널은 루트 변경과 원문·관측·실험의 각 커밋을 보여줍니다. 단계를 선택하면 해당 root와 seq의 상세를 엽니다. 3D 구성·버전 그래프는 전체 이력이며 관측·목표·평가는 같은 대시보드의 상세와 이력에서 조회합니다. 모든 입력·수정은 MCP를 이용합니다.

그래프 모델 회귀는 `node tests/graph-model.cjs`로 실행합니다. Playwright를 사용할 수 있는 Node 환경에서는 `node tests/dashboard.cjs <base-url> <lifecycle-report.json> <새-결과-디렉터리>`로 실제 UI를 검증합니다. 스크린샷과 실패 기록을 덮어쓰지 않습니다.
