#!/usr/bin/env bash

velnor_require_bool() {
  local name="$1"
  local value="$2"

  if [[ "$value" != "true" && "$value" != "false" ]]; then
    echo "$name must be 'true' or 'false'." >&2
    return 2
  fi
}

velnor_require_positive_int() {
  local name="$1"
  local value="$2"

  if ! [[ "$value" =~ ^[1-9][0-9]*$ ]]; then
    echo "$name must be a positive integer." >&2
    return 2
  fi
}

velnor_require_optional_positive_int() {
  local name="$1"
  local value="$2"

  if [[ -n "$value" ]]; then
    velnor_require_positive_int "$name" "$value"
  fi
}
