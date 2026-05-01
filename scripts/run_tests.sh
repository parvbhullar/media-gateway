#!/usr/bin/env bash
# Run test modules for rustpbx.
# Usage: bash scripts/run_tests.sh [module]
#        bash scripts/run_tests.sh list     — list available modules

set -euo pipefail

MODULES=(
    api_v1_manipulations
    proxy_manipulations_pipeline
    proxy_translation_engine
    api_v1_translations
    proxy_webhook_pipeline
    proxy_trunk_enforcement
    api_v1_routing_tables
    api_v1_routing_records
    api_v1_trunks
    api_v1_dids
    api_v1_calls
    api_v1_auth
)

if [[ "${1:-}" == "list" ]]; then
    printf '%s\n' "${MODULES[@]}"
    exit 0
fi

if [[ -n "${1:-}" ]]; then
    cargo test -p rustpbx --test "$1"
else
    for mod in "${MODULES[@]}"; do
        echo "=== $mod ==="
        cargo test -p rustpbx --test "$mod"
    done
fi
