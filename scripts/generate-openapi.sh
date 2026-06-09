#!/bin/bash
# Generate the canonical Hadrian OpenAPI spec, sync the UI client's copy, and
# regenerate the frontend SDK.
#
# The canonical spec (openapi/hadrian.openapi.json) is what the CI conformance
# job regenerates and diffs against, so this must update it — committing only
# the UI copy leaves CI failing.
#
# Usage: ./scripts/generate-openapi.sh [--no-build]

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SPEC_FILE="$PROJECT_ROOT/openapi/hadrian.openapi.json"
UI_SPEC_FILE="$PROJECT_ROOT/ui/src/api/openapi.json"

# Export the OpenAPI spec directly from the binary.
echo "Exporting OpenAPI spec..."
cargo run -- openapi --output "$SPEC_FILE"

# Keep the UI client's input copy byte-identical to the canonical spec.
cp "$SPEC_FILE" "$UI_SPEC_FILE"

# Generate the client SDK
cd "${PROJECT_ROOT}/ui" && pnpm run generate-api

echo "Done!"
