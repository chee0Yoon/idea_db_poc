.PHONY: check init-env up down logs acceptance docker-check lifecycle lifecycle-check evidence-check export
BASE_URL ?= http://127.0.0.1:8080
PYTHON ?= python3
REPORT_DIR ?= test-results/lifecycle-$(shell date +%Y%m%d-%H%M%S)

check:
	cargo fmt --check
	cargo clippy --locked --all-targets -- -D warnings
	cargo test --locked

init-env:
	sh scripts/init-env.sh

up: init-env
	docker compose up --build -d --wait --wait-timeout 180

down:
	docker compose down

logs:
	docker compose logs --tail 100 -f

acceptance:
	$(PYTHON) tests/acceptance.py --base-url $(BASE_URL)

docker-check:
	bash scripts/docker-check.sh

lifecycle:
	$(PYTHON) tests/lifecycle.py --base-url $(BASE_URL) --output-dir "$(REPORT_DIR)"

lifecycle-check:
	IDEA_DB_SUITE=lifecycle bash scripts/docker-check.sh

evidence-check:
	IDEA_DB_SUITE=evidence bash scripts/docker-check.sh

export:
	$(PYTHON) scripts/idea-db-client.py --url $(BASE_URL) export
