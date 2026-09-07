#!/usr/bin/env bash
# This script owns only resources bearing its unique run ID. Never resets a user DB.
set -Eeuo pipefail
cd "$(dirname "$0")/.."
test_run="idea-db-check-$(date +%s)-$$"
test_image="${IDEA_DB_TEST_IMAGE:-idea-db:acceptance}"
test_suite="${IDEA_DB_SUITE:-acceptance}"
case "$test_suite" in
  acceptance|lifecycle) ;;
  *) echo 'IDEA_DB_SUITE must be acceptance or lifecycle' >&2; exit 64 ;;
esac
lifecycle_dir="${IDEA_DB_REPORT_DIR:-$PWD/test-results/$test_run}"
test_dir=$(mktemp -d)
primary="${test_run}-primary"
restored="${test_run}-restored"
test_password=$(openssl rand -hex 24)
cleanup() {
  local status=$?
  trap - EXIT
  if ((status != 0)); then
    docker logs --tail 100 "$primary" 2>/dev/null || true
    docker logs --tail 100 "$restored" 2>/dev/null || true
  fi
  docker rm -f "$primary" "$restored" >/dev/null 2>&1 || true
  for suffix in primary-data primary-logs restored-data restored-logs; do
    docker volume rm "${test_run}-${suffix}" >/dev/null 2>&1 || true
  done
  rm -rf "$test_dir"
  exit "$status"
}
trap cleanup EXIT
umask 077
printf 'NEO4J_PASSWORD=%s\n' "$test_password" > "$test_dir/env"

wait_ready() {
  local container=$1
  local health
  for ((attempt=0; attempt<180; attempt++)); do
    health=$(docker inspect --format '{{.State.Running}} {{if .State.Health}}{{.State.Health.Status}}{{end}}' "$container")
    if [[ "$health" == 'true healthy' ]]; then return 0; fi
    if [[ "$health" == false* ]]; then
      echo "$container exited before readiness" >&2
      return 1
    fi
    sleep 1
  done
  echo "$container readiness timeout" >&2
  return 1
}
start_container() {
  local container=$1 suffix=$2
  docker run -d --name "$container" --label "idea_db.test=$test_run" \
    --env-file "$test_dir/env" -p 127.0.0.1::8080 \
    -v "${test_run}-${suffix}-data:/data" -v "${test_run}-${suffix}-logs:/logs" \
    "$test_image" >/dev/null
  wait_ready "$container"
}
base_for() { printf 'http://%s' "$(docker port "$1" 8080/tcp)"; }

if [[ ${IDEA_DB_SKIP_BUILD:-0} != 1 ]]; then
  docker build --target standalone -t "$test_image" .
fi
start_container "$primary" primary
primary_url=$(base_for "$primary")
if [[ "$test_suite" == lifecycle ]]; then
  python3 tests/lifecycle.py --base-url "$primary_url" --output-dir "$lifecycle_dir"
else
  python3 tests/acceptance.py --base-url "$primary_url"
fi
python3 scripts/idea-db-client.py --url "$primary_url" export --output "$test_dir/before.json"

# Abrupt process death exercises the database log recovery, not just graceful stop.
docker kill --signal KILL "$primary" >/dev/null
docker start "$primary" >/dev/null
wait_ready "$primary"
primary_url=$(base_for "$primary")
python3 scripts/idea-db-client.py --url "$primary_url" export --output "$test_dir/restarted.json"
python3 - "$test_dir/before.json" "$test_dir/restarted.json" <<'PY'
import json, sys
a, b = [json.load(open(p)) for p in sys.argv[1:]]
assert a['content'] == b['content'], 'Restart changed authoritative export content'
assert a['digest'] == b['digest'], 'Restart changed export digest'
print('PASS: abrupt container kill/restart preserves records, heads, receipts, sequence and digest')
PY
if [[ "$test_suite" == lifecycle ]]; then
  python3 tests/lifecycle.py --base-url "$primary_url" --verify-report "$lifecycle_dir/report.json"
fi

start_container "$restored" restored
restored_url=$(base_for "$restored")
python3 scripts/idea-db-client.py --url "$restored_url" import "$test_dir/before.json" >/dev/null
python3 scripts/idea-db-client.py --url "$restored_url" export --output "$test_dir/restored.json"
python3 - "$test_dir/before.json" "$test_dir/restored.json" <<'PY'
import json, sys
a, b = [json.load(open(p)) for p in sys.argv[1:]]
assert a['content'] == b['content'], 'Restore changed authoritative export content'
assert a['digest'] == b['digest'], 'Restore changed export digest'
print('PASS: fresh-volume restore preserves complete graph identity and history')
PY
if [[ "$test_suite" == lifecycle ]]; then
  python3 tests/lifecycle.py --base-url "$restored_url" --verify-report "$lifecycle_dir/report.json"
fi

# An API process loss must not leave a healthy-looking Neo4j-only appliance.
docker exec "$restored" bash -c 'kill -TERM "$(pgrep -x idea-db)"'
exited=0
for ((attempt=0; attempt<45; attempt++)); do
  if [[ $(docker inspect --format '{{.State.Running}}' "$restored") == false ]]; then
    exited=1
    break
  fi
  sleep 1
done
[[ $exited == 1 ]] || { echo 'Required child loss did not stop container' >&2; exit 1; }
[[ $(docker inspect --format '{{.State.ExitCode}}' "$restored") != 0 ]] || {
  echo 'Unexpected child loss reported clean exit' >&2; exit 1;
}
echo 'PASS: required API process exit stops appliance with failure status'

docker stop --time 40 "$primary" >/dev/null
[[ $(docker inspect --format '{{.State.ExitCode}}' "$primary") == 0 ]] || {
  echo 'Graceful stop failed' >&2; exit 1;
}
echo 'PASS: graceful container shutdown'
if [[ "$test_suite" == lifecycle ]]; then
  docker image inspect "$test_image" --format '{{.Id}}' > "$test_dir/image-id"
  python3 - "$lifecycle_dir" "$test_dir/image-id" <<'PY'
import datetime, json, pathlib, sys
out = pathlib.Path(sys.argv[1])
report = {
    'completed_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
    'image_id': pathlib.Path(sys.argv[2]).read_text().strip(),
    'suite': 'lifecycle_30',
    'abrupt_restart_export_identity': True,
    'abrupt_restart_history_reverified': True,
    'fresh_volume_restore_export_identity': True,
    'fresh_volume_restore_history_reverified': True,
    'required_child_exit_fails_container': True,
    'graceful_shutdown_exit_zero': True,
}
with (out / 'deployment-checks.json').open('x', encoding='utf-8') as file:
    json.dump(report, file, ensure_ascii=False, indent=2)
    file.write('\n')
print('Lifecycle reports retained at: ' + str(out))
PY
fi
echo 'Docker acceptance passed; temporary test containers and volumes will be removed.'
