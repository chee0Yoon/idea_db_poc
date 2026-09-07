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

한 명의 신뢰된 소유자가 여러 로컬 클라이언트로 쓰는 MVP입니다. 선택적 API bearer token을 지원하며 팀 권한, 인터넷 서비스 운영, HA는 후속 범위입니다. GUI의 AI 제안은 공식 평가와 별도로 표시하며 자동 모델 호출은 없습니다. 수동 JSON 패키지와 로컬 AI 스킬이 같은 입력 API를 사용합니다.

본문·관측·평가는 불변 기록입니다. 의미가 바뀌는 Idea는 파생으로 생성하고, 조합은 정확한 Revision을 고정합니다. 검색의 역할은 occurrence에 붙으며, 엄격 일치와 관련 문맥을 구분합니다. 임베딩이 없는 환경에서도 텍스트 검색으로 동작합니다.

## 개발 및 검증

Rust 1.98, 실제 Neo4j 5.26.30 Community로 개발합니다. Neo4j 저장소에 연결하는 환경 변수는 `NEO4J_URI`, `NEO4J_USER`, `NEO4J_PASSWORD`이고, API 바인딩은 `IDEA_DB_BIND`, 정적 파일은 `STATIC_DIR`입니다.

```sh
make check
make acceptance BASE_URL=http://127.0.0.1:8080
make docker-check
```

acceptance는 실행 중인 DB에 고유 ID를 가진 테스트 Project를 추가합니다. 사용자 데이터 초기화/삭제는 하지 않습니다. 독립적인 검증 DB에는 `make docker-check`를 사용하세요. Python 3의 표준 라이브러리는 **검증/클라이언트 도구**에만 사용하며 이미지 DB 런타임에는 필요하지 않습니다.

API만 별도로 배포하려면 `docker build --target api -t idea-db-api:local .`로 빌드하고 외부 Neo4j의 `NEO4J_URI`와 인증을 설정합니다. 기본 standalone 이미지와 API-only 이미지는 동일한 Rust 코드를 사용합니다.

## 구현 선택과 한계

Neo4j의 개별 노드와 타입 관계가 권위 저장소이며 응용 ID를 사용합니다. 5.26 LTS에 고정된 트랜잭션 API를 사용합니다. 해당 HTTP API는 [5.26에서 deprecated](https://neo4j.com/docs/http-api/current/transactions/)되었으므로 Neo4j 메이저 업그레이드 전에 드라이버 변경과 수용 검증이 필요합니다.

초기 정확성 검증을 위해 쓰기를 직렬화하고 탐색/입력 크기를 제한합니다. 소규모 검증 결과를 팀 규모 성능 보장으로 해석하지 않습니다. 상세 구현 한계와 실행한 검증은 [docs/validation.md](docs/validation.md)에 기록합니다.

Neo4j Community는 공식 [Docker 이미지](https://hub.docker.com/_/neo4j/)를 기반으로 하며, 해당 구성 요소의 라이선스와 공지는 원본 이미지/배포물에 포함됩니다. 응용 DB 엔진 자체를 Rust로 다시 구현하는 작업은 이번 범위에 포함하지 않습니다.
