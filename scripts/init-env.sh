#!/usr/bin/env sh
set -eu
cd "$(dirname "$0")/.."
if [ -e .env ]; then
  echo '.env already exists; preserving it.'
  exit 0
fi
umask 077
# noclobber avoids replacing a file created concurrently.
set -C
{ printf 'NEO4J_PASSWORD='; openssl rand -hex 24; printf 'IDEA_DB_PORT=8080\n'; } > .env
echo 'Created local .env with a random database password.'

