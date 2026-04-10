#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "${SCRIPT_DIR}/e2e-common.sh"

health_status="$(curl -s -o /tmp/fmcd-e2e-health.out -w '%{http_code}' "${FMCD_URL}/health")"
if [[ "${health_status}" == "200" ]]; then
  health="$(cat /tmp/fmcd-e2e-health.out)"
  assert_jq '.status == "healthy" or .overall_status == "healthy" or .healthy == true' "${health}"
  print_ok "health endpoint"
else
  printf 'warn: health endpoint returned HTTP %s; continuing with authenticated API checks\n' "${health_status}" >&2
fi

config="$(fmcd_get "/v2/admin/config")"
assert_jq 'type == "object"' "${config}"
print_ok "admin config"

federations="$(fmcd_get "/v2/admin/federations")"
assert_jq '.federationIds | type == "array"' "${federations}"
print_ok "admin federations"

federation_id="$(echo "${federations}" | jq -r '.federationIds[0] // empty')"
if [[ -z "${federation_id}" ]]; then
  echo "No federations configured in fmcd" >&2
  exit 1
fi

info="$(fmcd_get "/v2/admin/info")"
assert_jq --arg federation_id "${federation_id}" 'has($federation_id)' "${info}"
print_ok "admin info"

tracked="$(fmcd_post "/v2/admin/operations/tracked" "{\"federationId\":\"${federation_id}\",\"limit\":10}")"
assert_jq '.operations | type == "array"' "${tracked}"
print_ok "tracked operations list"

fedimint_ops="$(fmcd_post "/v2/admin/operations" "{\"federationId\":\"${federation_id}\",\"limit\":10}")"
assert_jq '.operations | type == "array"' "${fedimint_ops}"
print_ok "fedimint oplog operations list"

printf 'smoke test passed for federation %s\n' "${federation_id}"
