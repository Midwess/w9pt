#!/usr/bin/env bash
set -euo pipefail

repository_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
expected_provider_image=chrislusf/seaweedfs@sha256:f7cbc8bdbbf60a1aaba7d61784a3bdff3ec1e0657f6ad0b26d5b6ab2cd9d0dc6
provider_image=${PROVIDER_IMAGE:-$expected_provider_image}
postgres_image=${POSTGRES_IMAGE:-postgres@sha256:4ef4dbc939d61acea57712655ddb4b4ab27419c913f94cca0cd57cb3ea3c2280}
provider_port=${W9PT_SEAWEEDFS_PORT:-18333}
postgres_port=${W9PT_POSTGRES_PORT:-15432}
compose_project="w9pt-integration-$$"
compose_file="$repository_root/test/compose.yaml"
endpoint=${W9PT_TEST_S3_ENDPOINT:-http://127.0.0.1:$provider_port}
bucket=${W9PT_TEST_S3_BUCKET:-w9pt-test-bucket}
tcp_prefix=${W9PT_TEST_S3_PREFIX:-tcp/w9pt-s3-test-development}
websocket_prefix=${W9PT_TEST_WEBSOCKET_S3_PREFIX:-websocket/w9pt-s3-test-development}
compat_prefix=${W9PT_S3_COMPAT_TEST_PREFIX:-compat/target/w9pt-s3-test-0123456789abcdef}
compat_repository_prefix=${W9PT_S3_COMPAT_REPOSITORY_TEST_PREFIX:-compat/repository/w9pt-s3-test-fedcba9876543210}
postgres_dsn=${W9PT_POSTGRES_TEST_DSN:-postgres://w9pt_test:w9pt_test@127.0.0.1:$postgres_port/w9pt_test}
headers=$(mktemp)
export POSTGRES_IMAGE="$postgres_image"
export PROVIDER_IMAGE="$provider_image"
export W9PT_POSTGRES_PORT="$postgres_port"
export W9PT_SEAWEEDFS_PORT="$provider_port"

if [[ "$provider_image" != "$expected_provider_image" ]]; then
  echo "PROVIDER_IMAGE must equal the approved SeaweedFS 4.42 digest" >&2
  exit 1
fi

compose() {
  docker compose --project-name "$compose_project" --file "$compose_file" "$@"
}

cleanup() {
  local status=$?
  local teardown_status=0
  trap - EXIT INT TERM
  set +e
  if (( status != 0 )); then
    compose logs --no-color --tail 200 seaweedfs postgres >&2
  fi
  compose down --timeout 10 --volumes --remove-orphans || teardown_status=$?
  local remaining_containers
  local remaining_networks
  local remaining_volumes
  remaining_containers=$(docker ps --all --quiet --filter "label=com.docker.compose.project=$compose_project") || teardown_status=1
  remaining_networks=$(docker network ls --quiet --filter "label=com.docker.compose.project=$compose_project") || teardown_status=1
  remaining_volumes=$(docker volume ls --quiet --filter "label=com.docker.compose.project=$compose_project") || teardown_status=1
  if [[ -n "$remaining_containers$remaining_networks$remaining_volumes" ]]; then
    echo "Compose project $compose_project left scoped resources behind" >&2
    teardown_status=1
  fi
  rm -f "$headers"
  if (( status == 0 && teardown_status != 0 )); then
    status=$teardown_status
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

compose pull
compose up --detach --wait --wait-timeout 60

for attempt in $(seq 1 30); do
  if curl --connect-timeout 1 --max-time 2 --fail --silent --show-error "$endpoint/" >/dev/null; then
    break
  fi
  if [[ "$attempt" == 30 ]]; then
    compose logs seaweedfs
    exit 1
  fi
  sleep 1
done

for attempt in $(seq 1 30); do
  if compose exec --no-TTY postgres pg_isready --username w9pt_test --dbname w9pt_test >/dev/null; then
    break
  fi
  if [[ "$attempt" == 30 ]]; then
    compose logs postgres
    exit 1
  fi
  sleep 1
done

curl --connect-timeout 1 --max-time 2 --silent --show-error --dump-header "$headers" --output /dev/null "$endpoint/"
grep --fixed-strings 'Server: SeaweedFS 30GB 4.42' "$headers"
curl --connect-timeout 1 --max-time 2 --fail --silent --show-error --request PUT "$endpoint/$bucket"

cd "$repository_root"
W9PT_S3_COMPAT_TEST_REQUIRED=1 \
W9PT_S3_COMPAT_TEST_ENDPOINT="$endpoint" \
W9PT_S3_COMPAT_TEST_BUCKET="$bucket" \
W9PT_S3_COMPAT_TEST_PROVIDER=SeaweedFS \
W9PT_S3_COMPAT_TEST_VERSION=4.42 \
W9PT_S3_COMPAT_TEST_PREFIX="$compat_prefix" \
cargo test -p w9pt-fs-storage-s3 --test s3_conformance \
  live_pinned_compatible_provider_is_exercised_but_remains_unsupported \
  --locked -- --exact

W9PT_S3_COMPAT_REPOSITORY_TEST_REQUIRED=1 \
W9PT_S3_COMPAT_TEST_ENDPOINT="$endpoint" \
W9PT_S3_COMPAT_TEST_BUCKET="$bucket" \
W9PT_S3_COMPAT_TEST_PROVIDER=SeaweedFS \
W9PT_S3_COMPAT_TEST_VERSION=4.42 \
W9PT_S3_COMPAT_REPOSITORY_TEST_PREFIX="$compat_repository_prefix" \
cargo test -p w9pt-fs-storage-s3 --test s3_conformance \
  live_pinned_compatible_provider_repository_matrix_remains_unqualified \
  --locked -- --exact

W9PT_POSTGRES_TEST_DSN="$postgres_dsn" \
cargo test -p w9pt-fs-state-postgres --all-features --locked -- --test-threads=1

W9PT_TCP_SEAWEED_TEST_REQUIRED=1 \
W9PT_TEST_S3_ENDPOINT="$endpoint" \
W9PT_TEST_S3_BUCKET="$bucket" \
W9PT_TEST_S3_PREFIX="$tcp_prefix" \
cargo test --manifest-path test/Cargo.toml --locked --test tcp_seaweedfs -- --nocapture

W9PT_WEBSOCKET_SEAWEED_TEST_REQUIRED=1 \
W9PT_TEST_S3_ENDPOINT="$endpoint" \
W9PT_TEST_S3_BUCKET="$bucket" \
W9PT_TEST_WEBSOCKET_S3_PREFIX="$websocket_prefix" \
cargo test --manifest-path test/Cargo.toml --locked --test websocket_seaweedfs -- --nocapture
