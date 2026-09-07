#!/usr/bin/env bash
set -Eeuo pipefail

: "${NEO4J_PASSWORD:?Set NEO4J_PASSWORD (at least 8 characters)}"
if (( ${#NEO4J_PASSWORD} < 8 )); then
  echo 'NEO4J_PASSWORD must contain at least 8 characters' >&2
  exit 64
fi
export NEO4J_AUTH="neo4j/${NEO4J_PASSWORD}"
export NEO4J_USER=neo4j
export NEO4J_URI=http://127.0.0.1:7474
db_pid=''
api_pid=''
stop_children() {
  trap - TERM INT
  [[ -z "$api_pid" ]] || kill -TERM "$api_pid" 2>/dev/null || true
  [[ -z "$db_pid" ]] || kill -TERM "$db_pid" 2>/dev/null || true
  [[ -z "$api_pid" ]] || wait "$api_pid" 2>/dev/null || true
  [[ -z "$db_pid" ]] || wait "$db_pid" 2>/dev/null || true
}
trap 'stop_children; exit 0' TERM INT
trap stop_children EXIT

# Neo4j converts NEO4J_* variables into configuration keys. These four are
# Rust client settings, so keep them out of the database child environment.
env -u NEO4J_PASSWORD -u NEO4J_USER -u NEO4J_URI -u NEO4J_DATABASE \
    -u IDEA_DB_TOKEN /startup/docker-entrypoint.sh neo4j &
db_pid=$!
ready=0
for ((attempt=0; attempt<120; attempt++)); do
  if ! kill -0 "$db_pid" 2>/dev/null; then
    echo 'Neo4j exited during startup' >&2
    exit 1
  fi
  if wget -q --timeout=1 --tries=1 -O /dev/null http://127.0.0.1:7474/; then
    ready=1
    break
  fi
  sleep 1
done
if (( ! ready )); then
  echo 'Neo4j did not become ready within 120 seconds' >&2
  exit 1
fi

if [[ $(id -u) == 0 ]]; then
  su-exec neo4j /usr/local/bin/idea-db &
else
  /usr/local/bin/idea-db &
fi
api_pid=$!
set +e
wait -n "$db_pid" "$api_pid"
child_status=$?
set -e
echo "A required service exited (status ${child_status}); stopping container" >&2
# An unexpected clean child exit is still a failed appliance.
exit 1
