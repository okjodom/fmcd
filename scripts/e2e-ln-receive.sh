#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "${SCRIPT_DIR}/e2e-common.sh"

federation_id="$(get_first_federation_id)"
if [[ -z "${federation_id}" ]]; then
  echo "No federations configured in fmcd" >&2
  exit 1
fi

gateway_id="$(get_first_gateway_id "${federation_id}")"
if [[ -z "${gateway_id}" ]]; then
  echo "skip: no gateways available for federation ${federation_id}" >&2
  if [[ "${REQUIRE_LN_GATEWAY:-0}" == "1" ]]; then
    exit 1
  fi
  exit 0
fi

invoice_response="$(
  fmcd_post "/v2/ln/invoice" "{
    \"amountMsat\": 1000000,
    \"description\": \"e2e ln receive test\",
    \"expiryTime\": 3600,
    \"gatewayId\": \"${gateway_id}\",
    \"federationId\": \"${federation_id}\"
  }"
)"

assert_jq '.operationId and .invoice and .status' "${invoice_response}"
print_ok "invoice created"

operation_id="$(echo "${invoice_response}" | jq -r '.operationId')"

status_response="$(fmcd_get "/v2/ln/operation/${operation_id}/status?federationId=${federation_id}")"
assert_jq --arg operation_id "${operation_id}" '.operationId == $operation_id' "${status_response}"
assert_jq '.status == "created" or .status == "pending" or (.status | type == "object")' "${status_response}"
print_ok "single invoice status"

bulk_status="$(
  fmcd_post "/v2/ln/invoice/status/bulk" "{
    \"federationId\": \"${federation_id}\",
    \"operationIds\": [\"${operation_id}\"]
  }"
)"
assert_jq '.statuses | length == 1' "${bulk_status}"
assert_jq '.statuses[0].operationId' "${bulk_status}"
print_ok "bulk invoice status"

tracked_lookup="$(fmcd_get "/v2/admin/operations/tracked/${operation_id}")"
assert_jq --arg operation_id "${operation_id}" '.id == $operation_id' "${tracked_lookup}"
assert_jq '.paymentType == "LightningReceive"' "${tracked_lookup}"
print_ok "tracked operation lookup"

tracked_list="$(fmcd_post "/v2/admin/operations/tracked" "{\"federationId\":\"${federation_id}\",\"limit\":20}")"
assert_jq --arg operation_id "${operation_id}" '.operations | any(.id == $operation_id)' "${tracked_list}"
print_ok "tracked operation list contains invoice op"

printf 'ln receive test passed: federation=%s operation=%s\n' "${federation_id}" "${operation_id}"
