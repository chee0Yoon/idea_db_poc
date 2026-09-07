#!/usr/bin/env bash
# This script owns only resources bearing its unique run ID. Never resets a user DB.
set -Eeuo pipefail
cd "$(dirname "$0")/.."
test_run="idea-db-check-$(date +%s)-$$"
test_image="${IDEA_DB_TEST_IMAGE:-idea-db:acceptance}"
test_suite="${IDEA_DB_SUITE:-acceptance}"
case "$test_suite" in
  acceptance|lifecycle|evidence) ;;
  *) echo 'IDEA_DB_SUITE must be acceptance, lifecycle, or evidence' >&2; exit 64 ;;
esac
lifecycle_dir="${IDEA_DB_REPORT_DIR:-$PWD/test-results/$test_run}"
test_dir=$(mktemp -d)
export IDEA_DB_MCP_LOG_DIR="$lifecycle_dir/logs/$test_run/mcp"
primary="${test_run}-primary"
restored="${test_run}-restored"
test_password=$(openssl rand -hex 24)
# Bash 3 with `set -u` treats an empty array expansion as unbound.  Keep a
# harmless sentinel so cleanup can always scan this list on macOS.
preserved_log_volumes=("")

preserve_container_logs() {
  local container=$1
  local label=$2
  local volume=$3
  local destination="$lifecycle_dir/logs/$test_run/${label}-logs"

  # `/logs` is a mounted volume, so copy it while its owning container still
  # exists.  If that copy cannot be made, retain the volume for manual
  # recovery instead of deleting the only remaining log evidence.
  if ! docker inspect "$container" >/dev/null 2>&1; then
    return 0
  fi
  mkdir -p "$destination"
  if docker cp "$container:/logs/." "$destination/" >/dev/null 2>&1; then
    return 0
  fi
  printf 'Could not copy /logs from %s; retaining volume %s for recovery.\n' \
    "$container" "$volume" >&2
  preserved_log_volumes+=("$volume")
}

keep_log_volume() {
  local volume=$1
  local kept
  for kept in "${preserved_log_volumes[@]}"; do
    [[ "$kept" == "$volume" ]] && return 0
  done
  return 1
}

cleanup() {
  local status=$?
  trap - EXIT
  # Preserve logs on success and failure before removing only owned containers.
  mkdir -p "$lifecycle_dir/logs/$test_run"
  docker logs --timestamps "$primary" > "$lifecycle_dir/logs/$test_run/primary.log" 2>&1 || true
  docker logs --timestamps "$restored" > "$lifecycle_dir/logs/$test_run/restored.log" 2>&1 || true
  preserve_container_logs "$primary" primary "${test_run}-primary-logs"
  preserve_container_logs "$restored" restored "${test_run}-restored-logs"
  if ((status != 0)); then
    docker logs --tail 100 "$primary" 2>/dev/null || true
    docker logs --tail 100 "$restored" 2>/dev/null || true
  fi
  docker rm -f "$primary" "$restored" >/dev/null 2>&1 || true
  for suffix in primary-data primary-logs restored-data restored-logs; do
    volume="${test_run}-${suffix}"
    if keep_log_volume "$volume"; then
      printf 'Preserved Docker log volume: %s\n' "$volume" >&2
      continue
    fi
    docker volume rm "$volume" >/dev/null 2>&1 || true
  done
  rm -rf "$test_dir"
  exit "$status"
}
trap cleanup EXIT
umask 077
printf 'NEO4J_PASSWORD=%s\n' "$test_password" > "$test_dir/env"
printf 'IDEA_DB_EMBEDDING_URL=http://host.docker.internal:11434\nIDEA_DB_EMBEDDING_MODEL=embeddinggemma:300m\n' >> "$test_dir/env"

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
    --add-host host.docker.internal:host-gateway \
    --env-file "$test_dir/env" -p 127.0.0.1::8080 \
    -v "${test_run}-${suffix}-data:/data" -v "${test_run}-${suffix}-logs:/logs" \
    "$test_image" >/dev/null
  wait_ready "$container"
}
base_for() { printf 'http://%s' "$(docker port "$1" 8080/tcp)"; }
mcp_for() {
  python3 - "$1" <<'PY'
import json, sys
print(json.dumps(["docker", "exec", "-i", sys.argv[1], "idea-db-mcp"]))
PY
}

if [[ ${IDEA_DB_SKIP_BUILD:-0} != 1 ]]; then
  docker build --target standalone -t "$test_image" .
fi
start_container "$primary" primary
primary_url=$(base_for "$primary")
export IDEA_DB_MCP_COMMAND_JSON="$(mcp_for "$primary")"
if [[ "$test_suite" == evidence ]]; then
  python3 tests/acceptance.py --base-url "$primary_url"
  python3 tests/mcp_ingest.py --base-url "$primary_url" --output-dir "$lifecycle_dir/mcp-ingest"
  python3 tests/lifecycle.py --base-url "$primary_url" --output-dir "$lifecycle_dir/lifecycle"
  python3 tests/context_evidence.py --base-url "$primary_url" --output-dir "$lifecycle_dir/context"
  python3 tests/semantic_probes.py --base-url "$primary_url" --output-dir "$lifecycle_dir/semantic-probes"
  python3 tests/model_roundtrip.py --base-url "$primary_url" --output-dir "$lifecycle_dir/model-roundtrip"
  python3 tests/integrity_audit.py --base-url "$primary_url" --output-dir "$lifecycle_dir/audit-before"
elif [[ "$test_suite" == lifecycle ]]; then
  python3 tests/lifecycle.py --base-url "$primary_url" --output-dir "$lifecycle_dir"
else
  python3 tests/acceptance.py --base-url "$primary_url"
  python3 tests/mcp_ingest.py --base-url "$primary_url" --output-dir "$lifecycle_dir/mcp-ingest"
fi
python3 scripts/idea-db-client.py --url "$primary_url" export --output "$test_dir/before.json"
if [[ "$test_suite" == evidence ]]; then
  cp -n "$test_dir/before.json" "$lifecycle_dir/before-export.json"
fi

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
elif [[ "$test_suite" == evidence ]]; then
  cp -n "$test_dir/restarted.json" "$lifecycle_dir/restarted-export.json"
  python3 tests/lifecycle.py --base-url "$primary_url" --verify-report "$lifecycle_dir/lifecycle/report.json"
  python3 tests/context_evidence.py --base-url "$primary_url" --verify-manifest "$lifecycle_dir/context/manifest.json"
  python3 tests/integrity_audit.py --base-url "$primary_url" --output-dir "$lifecycle_dir/audit-restarted"
  python3 tests/semantic_probes.py --base-url "$primary_url" --output-dir "$lifecycle_dir/audit-restarted" --projection-only
fi

start_container "$restored" restored
restored_url=$(base_for "$restored")
export IDEA_DB_MCP_COMMAND_JSON="$(mcp_for "$restored")"
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
elif [[ "$test_suite" == evidence ]]; then
  cp -n "$test_dir/restored.json" "$lifecycle_dir/restored-export.json"
  python3 tests/lifecycle.py --base-url "$restored_url" --verify-report "$lifecycle_dir/lifecycle/report.json"
  python3 tests/context_evidence.py --base-url "$restored_url" --verify-manifest "$lifecycle_dir/context/manifest.json"
  python3 tests/integrity_audit.py --base-url "$restored_url" --output-dir "$lifecycle_dir/audit-restored"
  python3 tests/semantic_probes.py --base-url "$restored_url" --output-dir "$lifecycle_dir/audit-restored" --projection-only
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
if [[ "$test_suite" == lifecycle || "$test_suite" == evidence ]]; then
  docker image inspect "$test_image" --format '{{.Id}}' > "$test_dir/image-id"
  python3 - "$lifecycle_dir" "$test_dir/image-id" "$test_suite" <<'PY'
import datetime, json, pathlib, sys
out = pathlib.Path(sys.argv[1])
report = {
    'completed_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
    'image_id': pathlib.Path(sys.argv[2]).read_text().strip(),
    'suite': sys.argv[3],
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
