# `fmcd` Curl Examples

These examples are written for the `user` shell started by `just dev`.

That shell already exports:

- `FMCD_URL`
- `FMCD_PASS`
- `FMCD_ADDR`
- `FMCD_INVITE_CODE`

So the commands below are meant to be copy-pasted directly.

## Quick Setup

Confirm the shell env first:

```bash
printf 'FMCD_URL=%s\nFMCD_ADDR=%s\n' "$FMCD_URL" "$FMCD_ADDR"
```

Pick a federation id from the running instance:

```bash
export FEDERATION_ID="$(curl -s -u "fmcd:$FMCD_PASS" "$FMCD_URL/v2/admin/federations" | jq -r '.federationIds[0]')"
echo "$FEDERATION_ID"
```

Pick a gateway id when you need Lightning:

```bash
export GATEWAY_ID="$(
  curl -s -u "fmcd:$FMCD_PASS" \
    -X POST "$FMCD_URL/v2/ln/gateways" \
    -H "Content-Type: application/json" \
    -d "{\"federationId\":\"$FEDERATION_ID\"}" |
  jq -r '.[0].info.gateway_id // .[0].gateway_id // .[0].gatewayId'
)"
echo "$GATEWAY_ID"
```

## Admin

Get federation info:

```bash
curl -s -u "fmcd:$FMCD_PASS" "$FMCD_URL/v2/admin/info" | jq
```

List federations:

```bash
curl -s -u "fmcd:$FMCD_PASS" "$FMCD_URL/v2/admin/federations" | jq
```

List Fedimint oplog operations:

```bash
curl -s -u "fmcd:$FMCD_PASS" \
  -X POST "$FMCD_URL/v2/admin/operations" \
  -H "Content-Type: application/json" \
  -d "{
    \"federationId\": \"$FEDERATION_ID\",
    \"limit\": 50
  }" | jq
```

List normalized tracked operations:

```bash
curl -s -u "fmcd:$FMCD_PASS" \
  -X POST "$FMCD_URL/v2/admin/operations/tracked" \
  -H "Content-Type: application/json" \
  -d "{
    \"federationId\": \"$FEDERATION_ID\",
    \"limit\": 50
  }" | jq
```

Look up one normalized tracked operation:

```bash
export OPERATION_ID="<operation-id>"
curl -s -u "fmcd:$FMCD_PASS" \
  "$FMCD_URL/v2/admin/operations/tracked/$OPERATION_ID" | jq
```

Get config:

```bash
curl -s -u "fmcd:$FMCD_PASS" "$FMCD_URL/v2/admin/config" | jq
```

Get version:

```bash
curl -s -u "fmcd:$FMCD_PASS" "$FMCD_URL/v2/admin/version" | jq
```

Join a federation:

```bash
curl -s -u "fmcd:$FMCD_PASS" \
  -X POST "$FMCD_URL/v2/admin/join" \
  -H "Content-Type: application/json" \
  -d "{
    \"inviteCode\": \"$FMCD_INVITE_CODE\"
  }" | jq
```

Create a federation backup:

```bash
curl -s -u "fmcd:$FMCD_PASS" \
  -X POST "$FMCD_URL/v2/admin/backup" \
  -H "Content-Type: application/json" \
  -d "{
    \"federationId\": \"$FEDERATION_ID\",
    \"metadata\": {}
  }" | jq
```

## Lightning

List gateways:

```bash
curl -s -u "fmcd:$FMCD_PASS" \
  -X POST "$FMCD_URL/v2/ln/gateways" \
  -H "Content-Type: application/json" \
  -d "{
    \"federationId\": \"$FEDERATION_ID\"
  }" | jq
```

Create an invoice:

```bash
curl -s -u "fmcd:$FMCD_PASS" \
  -X POST "$FMCD_URL/v2/ln/invoice" \
  -H "Content-Type: application/json" \
  -d "{
    \"amountMsat\": 1000000,
    \"description\": \"Test invoice\",
    \"expiryTime\": 3600,
    \"gatewayId\": \"$GATEWAY_ID\",
    \"federationId\": \"$FEDERATION_ID\"
  }" | jq
```

Create an invoice and save the operation id:

```bash
INVOICE_RESPONSE="$(
  curl -s -u "fmcd:$FMCD_PASS" \
    -X POST "$FMCD_URL/v2/ln/invoice" \
    -H "Content-Type: application/json" \
    -d "{
      \"amountMsat\": 1000000,
      \"description\": \"Tracked status test\",
      \"expiryTime\": 3600,
      \"gatewayId\": \"$GATEWAY_ID\",
      \"federationId\": \"$FEDERATION_ID\"
    }"
)"
export OPERATION_ID="$(echo "$INVOICE_RESPONSE" | jq -r '.operationId')"
echo "$INVOICE_RESPONSE" | jq
echo "$OPERATION_ID"
```

Check Lightning status by operation id:

```bash
curl -s -u "fmcd:$FMCD_PASS" \
  "$FMCD_URL/v2/ln/operation/$OPERATION_ID/status?federationId=$FEDERATION_ID" | jq
```

Bulk-check Lightning status:

```bash
curl -s -u "fmcd:$FMCD_PASS" \
  -X POST "$FMCD_URL/v2/ln/invoice/status/bulk" \
  -H "Content-Type: application/json" \
  -d "{
    \"federationId\": \"$FEDERATION_ID\",
    \"operationIds\": [\"$OPERATION_ID\"]
  }" | jq
```

Pay an invoice:

```bash
export BOLT11="<bolt11-invoice>"
curl -s -u "fmcd:$FMCD_PASS" \
  -X POST "$FMCD_URL/v2/ln/pay" \
  -H "Content-Type: application/json" \
  -d "{
    \"paymentInfo\": \"$BOLT11\",
    \"gatewayId\": \"$GATEWAY_ID\",
    \"federationId\": \"$FEDERATION_ID\"
  }" | jq
```

## On-chain

Get a deposit address:

```bash
curl -s -u "fmcd:$FMCD_PASS" \
  -X POST "$FMCD_URL/v2/onchain/deposit-address" \
  -H "Content-Type: application/json" \
  -d "{
    \"federationId\": \"$FEDERATION_ID\"
  }" | jq
```

Await a deposit:

```bash
export OPERATION_ID="<deposit-operation-id>"
curl -s -u "fmcd:$FMCD_PASS" \
  -X POST "$FMCD_URL/v2/onchain/await-deposit" \
  -H "Content-Type: application/json" \
  -d "{
    \"operationId\": \"$OPERATION_ID\",
    \"federationId\": \"$FEDERATION_ID\"
  }" | jq
```

Withdraw on-chain:

```bash
export BTC_ADDRESS="<bitcoin-address>"
curl -s -u "fmcd:$FMCD_PASS" \
  -X POST "$FMCD_URL/v2/onchain/withdraw" \
  -H "Content-Type: application/json" \
  -d "{
    \"address\": \"$BTC_ADDRESS\",
    \"amountSat\": 50000,
    \"federationId\": \"$FEDERATION_ID\"
  }" | jq
```

## Mint

Split notes:

```bash
export NOTES="<notes>"
curl -s -u "fmcd:$FMCD_PASS" \
  -X POST "$FMCD_URL/v2/mint/split" \
  -H "Content-Type: application/json" \
  -d "{
    \"notes\": \"$NOTES\"
  }" | jq
```

Combine notes:

```bash
export NOTE1="<notes-1>"
export NOTE2="<notes-2>"
curl -s -u "fmcd:$FMCD_PASS" \
  -X POST "$FMCD_URL/v2/mint/combine" \
  -H "Content-Type: application/json" \
  -d "{
    \"notesVec\": [\"$NOTE1\", \"$NOTE2\"]
  }" | jq
```

Spend notes:

```bash
curl -s -u "fmcd:$FMCD_PASS" \
  -X POST "$FMCD_URL/v2/mint/spend" \
  -H "Content-Type: application/json" \
  -d "{
    \"amountMsat\": 100000,
    \"allowOverpay\": true,
    \"timeout\": 60,
    \"includeInvite\": false,
    \"federationId\": \"$FEDERATION_ID\"
  }" | jq
```

Validate notes:

```bash
export NOTES="<notes>"
curl -s -u "fmcd:$FMCD_PASS" \
  -X POST "$FMCD_URL/v2/mint/validate" \
  -H "Content-Type: application/json" \
  -d "{
    \"notes\": \"$NOTES\"
  }" | jq
```

Reissue notes:

```bash
export NOTES="<notes>"
curl -s -u "fmcd:$FMCD_PASS" \
  -X POST "$FMCD_URL/v2/mint/reissue" \
  -H "Content-Type: application/json" \
  -d "{
    \"notes\": \"$NOTES\"
  }" | jq
```

## WebSocket

`wscat` only works when `fmcd` is started in websocket mode, for example:

```bash
FMCD_MODE=ws cargo run -- --data-dir ./fmcd-data --password "$FMCD_PASS" --addr "$FMCD_ADDR"
```

Then connect:

```bash
wscat -c "ws://${FMCD_ADDR}/ws" -H "Authorization: Bearer $FMCD_PASS"
```

Example request:

```json
{"method":"admin.info","params":{},"id":1}
```

## Useful Checks For This Branch

After creating LN or on-chain activity, compare the normalized tracked record with status output:

```bash
curl -s -u "fmcd:$FMCD_PASS" \
  "$FMCD_URL/v2/admin/operations/tracked/$OPERATION_ID" | jq
```

```bash
curl -s -u "fmcd:$FMCD_PASS" \
  "$FMCD_URL/v2/ln/operation/$OPERATION_ID/status?federationId=$FEDERATION_ID" | jq
```

Things to verify:

1. `feeMsat` is present for outgoing LN payments when known.
2. `trackedStatus` is present in LN status responses.
3. `/v2/admin/operations/tracked` returns persisted normalized history.
4. `/v2/admin/operations/tracked/:operation_id` returns the same operation details directly.

## Known Limits

These endpoints exist but are not useful for copy-paste live testing right now:

- `/v2/admin/module`: not implemented
- `/v2/admin/restore`: placeholder behavior
