#!/usr/bin/env bash

set -euo pipefail

if [[ -f ./.env ]]; then
  set -a
  # shellcheck disable=SC1091
  . ./.env
  set +a
fi

export FMCD_URL="${FMCD_URL:-http://${FMCD_ADDR:-127.0.0.1:7070}}"
export FMCD_PASS="${FMCD_PASS:-${FMCD_PASSWORD:-}}"

if [[ -z "${FMCD_PASS}" ]]; then
  echo "FMCD_PASS or FMCD_PASSWORD must be set" >&2
  exit 1
fi

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "Missing required command: $1" >&2
    exit 1
  }
}

require_cmd curl
require_cmd jq

fmcd_get() {
  local path="$1"
  curl -fsS -u "fmcd:${FMCD_PASS}" "${FMCD_URL}${path}"
}

fmcd_post() {
  local path="$1"
  local body="$2"
  curl -fsS -u "fmcd:${FMCD_PASS}" \
    -X POST "${FMCD_URL}${path}" \
    -H "Content-Type: application/json" \
    -d "${body}"
}

assert_jq() {
  local input="${*: -1}"
  local jq_arg_count=$(( $# - 1 ))
  local jq_args=("${@:1:${jq_arg_count}}")
  echo "${input}" | jq -e "${jq_args[@]}" >/dev/null
}

get_first_federation_id() {
  fmcd_get "/v2/admin/federations" | jq -r '.federationIds[0] // empty'
}

get_first_gateway_id() {
  local federation_id="$1"
  fmcd_post "/v2/ln/gateways" "{\"federationId\":\"${federation_id}\"}" |
    jq -r '.[0].info.gateway_id // .[0].gateway_id // .[0].gatewayId // empty'
}

print_ok() {
  printf 'ok: %s\n' "$1"
}
