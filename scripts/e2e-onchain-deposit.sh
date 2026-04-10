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

deposit_response="$(
  fmcd_post "/v2/onchain/deposit-address" "{
    \"federationId\": \"${federation_id}\"
  }"
)"

assert_jq '.address and .operationId and .tweakIdx != null' "${deposit_response}"
print_ok "deposit address created"

operation_id="$(echo "${deposit_response}" | jq -r '.operationId')"

tracked_lookup="$(fmcd_get "/v2/admin/operations/tracked/${operation_id}")"
assert_jq --arg operation_id "${operation_id}" '.id == $operation_id' "${tracked_lookup}"
assert_jq '.paymentType == "OnchainDeposit"' "${tracked_lookup}"
assert_jq '.status == "Created" or .status == "Pending"' "${tracked_lookup}"
print_ok "tracked onchain deposit lookup"

tracked_list="$(fmcd_post "/v2/admin/operations/tracked" "{\"federationId\":\"${federation_id}\",\"limit\":20}")"
assert_jq --arg operation_id "${operation_id}" '.operations | any(.id == $operation_id)' "${tracked_list}"
print_ok "tracked operations list contains deposit op"

printf 'onchain deposit test passed: federation=%s operation=%s\n' "${federation_id}" "${operation_id}"
