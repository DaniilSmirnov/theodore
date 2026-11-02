#!/bin/bash
# Fake `claude -p --input-format stream-json --replay-user-messages` for
# process tests (crates/agents/tests/process.rs): each message read from
# stdin is echoed back as taken; a turn "works" for WORK seconds and takes
# whatever else arrives meanwhile, like the real CLI between tool calls.
# Then one result per turn; exit 0 on EOF.

work="${1:-1}"  # whole seconds: macOS bash 3.2 has no fractional `read -t`
n=0
echo '{"type":"system","subtype":"init","session_id":"S"}'
while IFS= read -r line; do
  n=$((n + 1))
  taken=1
  echo "${line%\}},\"isReplay\":true}"
  while IFS= read -r -t "$work" more; do
    taken=$((taken + 1))
    echo "${more%\}},\"isReplay\":true}"
  done
  echo "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"turn $n took $taken\",\"session_id\":\"S\"}"
done
exit 0
