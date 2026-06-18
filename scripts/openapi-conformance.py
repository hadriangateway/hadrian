#!/usr/bin/env -S uv run
# /// script
# requires-python = ">=3.12"
# dependencies = [
#     "pyyaml>=6.0",
# ]
# ///
"""
OpenAPI Conformance Checker

Compares Hadrian's OpenAPI spec against OpenAI's official spec to ensure
API compatibility. Hadrian extensions are identified and documented.

Usage:
    ./scripts/openapi-conformance.py                    # Run full conformance check
    ./scripts/openapi-conformance.py --format json      # Output JSON report
    ./scripts/openapi-conformance.py --endpoint /chat/completions  # Check specific endpoint
    ./scripts/openapi-conformance.py --verbose          # Show detailed differences

CI Pass/Fail Criteria:
    1. No missing endpoints (all OpenAI endpoints must be implemented)
    2. Missing fields must be documented in DOCUMENTED_MISSING_FIELDS with a reason
    3. Extension fields must have "**Hadrian Extension:**" in their OpenAPI description
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path
from typing import Any

# =============================================================================
# DOCUMENTED MISSING FIELDS
# =============================================================================
# Fields that are intentionally not implemented in Hadrian, with reasons.
# Format: (openai_path, method, location, field) -> reason
# location is "request", "response", or "param"
#
# CI will FAIL if a missing field is not documented here.
# =============================================================================

DOCUMENTED_MISSING_FIELDS: dict[tuple[str, str, str, str], str] = {
    # /chat/completions - OpenAI-specific or deprecated fields
    ("/chat/completions", "POST", "request", "safety_identifier"): "OpenAI internal safety feature",
    ("/chat/completions", "POST", "request", "prompt_cache_key"): "OpenAI-specific prompt caching key",
    ("/chat/completions", "POST", "request", "service_tier"): "OpenAI service tier selection (auto/default)",
    ("/chat/completions", "POST", "request", "prompt_cache_retention"): "OpenAI-specific cache retention",
    ("/chat/completions", "POST", "request", "modalities"): "OpenAI multimodal output selection",
    ("/chat/completions", "POST", "request", "verbosity"): "OpenAI-specific verbosity control",
    ("/chat/completions", "POST", "request", "reasoning_effort"): "OpenAI o1/o3 reasoning - Hadrian uses 'reasoning' object instead",
    ("/chat/completions", "POST", "request", "web_search_options"): "OpenAI web search - Hadrian has separate web_search feature",
    ("/chat/completions", "POST", "request", "audio"): "OpenAI audio output in chat",
    ("/chat/completions", "POST", "request", "store"): "OpenAI stored completions feature",
    ("/chat/completions", "POST", "request", "n"): "Multiple completions - not supported for cost/complexity reasons",
    ("/chat/completions", "POST", "request", "prediction"): "OpenAI predicted outputs feature",
    ("/chat/completions", "POST", "request", "parallel_tool_calls"): "OpenAI parallel tool execution hint",
    ("/chat/completions", "POST", "request", "function_call"): "Deprecated - use tool_choice instead",
    ("/chat/completions", "POST", "request", "functions"): "Deprecated - use tools instead",
    ("/chat/completions", "POST", "request", "include_obfuscation"): "OpenAI internal obfuscation feature",
    ("/chat/completions", "POST", "request", "moderation"): "OpenAI hosted moderation pass (omni-moderation) - Hadrian has separate guardrails feature",
    # /completions - Legacy endpoint, minimal support
    ("/completions", "POST", "request", "include_usage"): "Legacy completions - use chat/completions instead",
    ("/completions", "POST", "request", "include_obfuscation"): "OpenAI internal obfuscation feature",
    # /images/* - Token details not tracked at this granularity
    ("/images/edits", "POST", "response", "output_tokens_details"): "Detailed token breakdown not tracked",
    ("/images/generations", "POST", "response", "output_tokens_details"): "Detailed token breakdown not tracked",
    ("/images/variations", "POST", "response", "output_tokens_details"): "Detailed token breakdown not tracked",
    # /models - Object field missing (schema issue)
    ("/models", "GET", "response", "object"): "List response object type - TODO: add to schema",
    # /responses - OpenAI-specific features
    ("/responses", "POST", "request", "moderation"): "OpenAI hosted moderation pass (omni-moderation) - Hadrian has separate guardrails feature",
    ("/responses", "POST", "request", "top_logprobs"): "Log probabilities not implemented",
    ("/responses", "POST", "request", "prompt_cache_retention"): "OpenAI-specific cache retention",
    ("/responses", "POST", "request", "max_tool_calls"): "Tool call limits not implemented",
    ("/responses", "POST", "request", "stream_options"): "Stream options not implemented",
    ("/responses", "POST", "request", "conversation"): "OpenAI conversation context feature",
    ("/responses", "POST", "request", "format"): "OpenAI-specific format parameter",
    ("/responses", "POST", "request", "verbosity"): "OpenAI-specific verbosity control",
    ("/responses", "POST", "request", "effort"): "OpenAI-specific effort parameter",
    ("/responses", "POST", "request", "summary"): "OpenAI-specific summary parameter",
    ("/responses", "POST", "request", "generate_summary"): "OpenAI-specific summary generation",
    ("/responses", "POST", "request", "context"): "OpenAI reasoning context mode (which reasoning items render on later turns) - not implemented",
    ("/responses", "POST", "request", "id"): "OpenAI-specific ID parameter",
    ("/responses", "POST", "request", "version"): "OpenAI-specific version parameter",
    ("/responses", "POST", "request", "variables"): "OpenAI-specific variables parameter",
    ("/responses", "POST", "request", "context_management"): "OpenAI-specific context management",
    # /responses tools[] - OpenAI-hosted tool variants not relevant to a gateway
    ("/responses", "POST", "request", "tools[apply_patch]"): "OpenAI hosted apply_patch tool",
    ("/responses", "POST", "request", "tools[code_interpreter]"): "OpenAI hosted code interpreter",
    ("/responses", "POST", "request", "tools[computer]"): "OpenAI computer-use tool",
    ("/responses", "POST", "request", "tools[computer_use_preview]"): "OpenAI computer-use preview tool",
    ("/responses", "POST", "request", "tools[custom]"): "OpenAI custom tool (non-strict args)",
    ("/responses", "POST", "request", "tools[image_generation]"): "OpenAI hosted image generation",
    ("/responses", "POST", "request", "tools[local_shell]"): "OpenAI hosted local shell — Hadrian uses the `shell` variant",
    ("/responses", "POST", "request", "tools[namespace]"): "OpenAI namespace tool",
    ("/responses", "POST", "request", "tools[tool_search]"): "OpenAI tool-search feature",
    # /responses tools[mcp] field - OpenAI-hosted MCP feature
    ("/responses", "POST", "request", "tunnel_id"): "OpenAI Secure MCP Tunnel ID for tools[mcp] - hosted MCP feature not relevant to a gateway",
    # /responses/compact - OpenAI-specific fields
    ("/responses/compact", "POST", "request", "prompt_cache_retention"): "OpenAI-specific cache retention",
    ("/responses/compact", "POST", "request", "service_tier"): "OpenAI service tier selection (auto/default)",
    # /responses/{response_id} - OpenAI-specific stored-response features
    ("/responses/{response_id}", "GET", "param", "include"): "OpenAI stored-response include filters",
    ("/responses/{response_id}", "GET", "param", "include_obfuscation"): "OpenAI internal obfuscation feature",
    # /containers - OpenAI-specific list filters / pagination knobs
    ("/containers", "GET", "param", "order"): "OpenAI sort-order param - Hadrian uses cursor pagination",
    ("/containers", "GET", "param", "name"): "OpenAI name filter - not implemented",
    ("/containers", "POST", "request", "type"): "Recursed from network_policy (opaque object in Hadrian's spec; discriminator only meaningful inside OpenAI's oneOf)",
    ("/containers/{container_id}/files", "GET", "param", "order"): "OpenAI sort-order param - Hadrian uses cursor pagination",
    ("/containers/{container_id}/files", "GET", "param", "after"): "OpenAI offset cursor - Hadrian uses opaque cursor pagination",
    # /vector_stores - OpenAI-specific features
    ("/vector_stores/{vector_store_id}/files", "POST", "request", "attributes"): "File attributes not implemented",
    ("/vector_stores/{vector_store_id}/files", "POST", "response", "static"): "Static file info not implemented",
    ("/vector_stores/{vector_store_id}/files/{file_id}", "GET", "response", "static"): "Static file info not implemented",
    ("/vector_stores/{vector_store_id}/search", "POST", "request", "rewrite_query"): "Query rewriting not implemented",
    ("/vector_stores/{vector_store_id}/search", "POST", "response", "search_query"): "Rewritten query not returned",
    ("/vector_stores/{vector_store_id}/search", "POST", "response", "has_more"): "Pagination not implemented for search",
    ("/vector_stores/{vector_store_id}/search", "POST", "response", "next_page"): "Pagination not implemented for search",
}

# =============================================================================
# EXTENSION FIELD MARKER
# =============================================================================
# All Hadrian extension fields must have this marker in their OpenAPI description.
# This ensures extensions are documented and discoverable.
# =============================================================================

EXTENSION_MARKER = "**Hadrian Extension:**"


class DiffType(Enum):
    """Type of difference found between specs."""
    MISSING_IN_HADRIAN = "missing_in_hadrian"
    HADRIAN_EXTENSION = "hadrian_extension"
    TYPE_MISMATCH = "type_mismatch"
    REQUIRED_MISMATCH = "required_mismatch"


@dataclass
class SchemaDiff:
    """A single schema difference."""
    path: str
    field: str
    diff_type: DiffType
    openai_value: Any = None
    hadrian_value: Any = None
    description: str = ""


@dataclass
class EndpointDiff:
    """Differences for a single endpoint."""
    path: str
    method: str
    request_diffs: list[SchemaDiff] = field(default_factory=list)
    response_diffs: list[SchemaDiff] = field(default_factory=list)
    param_diffs: list[SchemaDiff] = field(default_factory=list)
    missing_in_hadrian: bool = False
    hadrian_extension: bool = False


@dataclass
class Violation:
    """A CI-blocking violation."""
    violation_type: str  # "undocumented_missing", "unmarked_extension", "missing_endpoint"
    path: str
    method: str
    field: str = ""
    location: str = ""  # "request", "response", "param"
    message: str = ""


@dataclass
class ConformanceReport:
    """Full conformance report."""
    openai_version: str
    hadrian_version: str
    total_openai_endpoints: int
    total_hadrian_endpoints: int
    endpoints_checked: int
    fully_conformant: int
    endpoints_with_diffs: list[EndpointDiff] = field(default_factory=list)
    out_of_scope_endpoints: list[str] = field(default_factory=list)
    hadrian_only_endpoints: list[str] = field(default_factory=list)
    # CI violations
    violations: list[Violation] = field(default_factory=list)


def _is_array_type(schema: dict[str, Any]) -> bool:
    """True if the schema declares `type: array` in either OpenAPI 3.0
    (`"array"`) or 3.1 (`["array", "null"]`) form."""
    if not isinstance(schema, dict):
        return False
    t = schema.get("type")
    if t == "array":
        return True
    if isinstance(t, list) and "array" in t:
        return True
    return False


def _extract_type_const(schema: dict[str, Any]) -> str | None:
    """Pull the literal `type` value out of a tool-variant schema.

    Returns the string when `schema.properties.type` constrains the
    value to a single literal (via `enum: ["..."]` or `const: "..."`).
    Returns `None` otherwise — caller treats the union as
    non-discriminated and falls back to single-variant comparison.
    """
    if not isinstance(schema, dict):
        return None
    type_prop = schema.get("properties", {}).get("type")
    if not isinstance(type_prop, dict):
        return None
    if isinstance(type_prop.get("const"), str):
        return type_prop["const"]
    enum_values = type_prop.get("enum")
    if isinstance(enum_values, list) and len(enum_values) == 1 and isinstance(enum_values[0], str):
        return enum_values[0]
    return None


class OpenAPIResolver:
    """Resolves $ref and allOf in OpenAPI schemas."""

    def __init__(self, spec: dict[str, Any]):
        self.spec = spec
        self.components = spec.get("components", {}).get("schemas", {})
        self._cache: dict[str, dict] = {}

    def resolve_ref(self, ref: str) -> dict[str, Any]:
        """Resolve a $ref to its schema."""
        if ref in self._cache:
            return self._cache[ref]

        # Parse ref like "#/components/schemas/Foo"
        if not ref.startswith("#/"):
            return {}

        parts = ref[2:].split("/")
        result = self.spec
        for part in parts:
            if isinstance(result, dict):
                result = result.get(part, {})
            else:
                return {}

        # Pre-cache the raw schema so a recursive reference back to this
        # ref (e.g. discriminated unions whose variants embed the parent
        # type) short-circuits instead of looping. Once resolution
        # completes we overwrite with the fully-resolved form.
        self._cache[ref] = result if isinstance(result, dict) else {}
        resolved = self.resolve_schema(result)
        self._cache[ref] = resolved
        return resolved

    def resolve_schema(self, schema: dict[str, Any]) -> dict[str, Any]:
        """Fully resolve a schema, handling $ref, allOf, oneOf, anyOf.

        For `oneOf`/`anyOf` with at least one variant whose `type`
        property pins a literal value (e.g. `type.enum = ["mcp"]`), the
        result keeps a `_union_variants` map keyed by that literal so
        downstream code can compare each variant pairwise. Falls back
        to the old "first option" behavior for unions without
        discriminators (so generic shapes like `string | null` keep
        working).
        """
        if not isinstance(schema, dict):
            return schema

        # Handle $ref
        if "$ref" in schema:
            resolved = self.resolve_ref(schema["$ref"])
            # Merge any additional properties from the schema
            result = dict(resolved)
            for key, value in schema.items():
                if key != "$ref":
                    result[key] = value
            return result

        # Handle allOf - merge all schemas
        if "allOf" in schema:
            merged: dict[str, Any] = {
                "type": "object",
                "properties": {},
                "required": [],
            }
            for sub_schema in schema["allOf"]:
                resolved = self.resolve_schema(sub_schema)
                self._merge_schemas(merged, resolved)
            # Copy other fields
            for key, value in schema.items():
                if key != "allOf":
                    merged[key] = value
            return merged

        # Handle oneOf/anyOf. When every non-null variant pins a
        # literal `type` value, keep the full union structure so
        # `_compare_schemas` can match variants by discriminator across
        # specs. Otherwise fall back to "first option" for primitive
        # `string | null`-style unions.
        if "oneOf" in schema or "anyOf" in schema:
            options = schema.get("oneOf") or schema.get("anyOf", [])
            non_null_options = [
                opt for opt in options
                if not (isinstance(opt, dict) and opt.get("type") == "null")
            ]
            if not non_null_options:
                return {"type": "null", "_nullable": True}

            resolved_variants = [self.resolve_schema(opt) for opt in non_null_options]
            variant_map: dict[str, dict] = {}
            for variant in resolved_variants:
                type_value = _extract_type_const(variant)
                if type_value is not None:
                    variant_map[type_value] = variant

            if variant_map:
                # Discriminated union — keep all variants for pairwise
                # comparison. Pick the first variant as the "fallback"
                # shape for callers that don't understand `_union_variants`
                # (matches the old behavior).
                fallback = dict(resolved_variants[0])
                fallback["_nullable"] = True
                fallback["_union_variants"] = variant_map
                return fallback

            # Non-discriminated union — old behavior.
            resolved = resolved_variants[0]
            result = dict(resolved)
            result["_nullable"] = True
            return result

        # Resolve nested properties
        result = dict(schema)
        if "properties" in result:
            result["properties"] = {
                key: self.resolve_schema(value)
                for key, value in result["properties"].items()
            }
        if "items" in result:
            result["items"] = self.resolve_schema(result["items"])
        if "additionalProperties" in result and isinstance(result["additionalProperties"], dict):
            result["additionalProperties"] = self.resolve_schema(result["additionalProperties"])

        return result

    def _merge_schemas(self, target: dict, source: dict) -> None:
        """Merge source schema into target."""
        if "properties" in source:
            target.setdefault("properties", {}).update(
                self.resolve_schema({"properties": source["properties"]}).get("properties", {})
            )
        if "required" in source:
            existing = set(target.get("required", []))
            existing.update(source["required"])
            target["required"] = list(existing)
        # Copy other fields
        for key in ["type", "description"]:
            if key in source:
                target[key] = source[key]


class ConformanceChecker:
    """Compare OpenAI and Hadrian OpenAPI specs."""

    # OpenAI path -> Hadrian path mapping
    PATH_MAPPING = {
        "/chat/completions": "/api/v1/chat/completions",
        "/completions": "/api/v1/completions",
        "/embeddings": "/api/v1/embeddings",
        "/models": "/api/v1/models",
        "/models/{model}": "/api/v1/models/{model}",
        "/files": "/api/v1/files",
        "/files/{file_id}": "/api/v1/files/{file_id}",
        "/files/{file_id}/content": "/api/v1/files/{file_id}/content",
        "/audio/speech": "/api/v1/audio/speech",
        "/audio/transcriptions": "/api/v1/audio/transcriptions",
        "/audio/translations": "/api/v1/audio/translations",
        "/images/generations": "/api/v1/images/generations",
        "/images/edits": "/api/v1/images/edits",
        "/images/variations": "/api/v1/images/variations",
        "/moderations": "/api/v1/moderations",
        "/vector_stores": "/api/v1/vector_stores",
        "/vector_stores/{vector_store_id}": "/api/v1/vector_stores/{vector_store_id}",
        "/vector_stores/{vector_store_id}/files": "/api/v1/vector_stores/{vector_store_id}/files",
        "/vector_stores/{vector_store_id}/files/{file_id}": "/api/v1/vector_stores/{vector_store_id}/files/{file_id}",
        "/vector_stores/{vector_store_id}/search": "/api/v1/vector_stores/{vector_store_id}/search",
        "/responses": "/api/v1/responses",
        "/responses/compact": "/api/v1/responses/compact",
        "/responses/{response_id}": "/api/v1/responses/{response_id}",
        "/responses/{response_id}/cancel": "/api/v1/responses/{response_id}/cancel",
        "/containers": "/api/v1/containers",
        "/containers/{container_id}": "/api/v1/containers/{container_id}",
        "/containers/{container_id}/files": "/api/v1/containers/{container_id}/files",
        "/containers/{container_id}/files/{file_id}": "/api/v1/containers/{container_id}/files/{file_id}",
        "/containers/{container_id}/files/{file_id}/content": "/api/v1/containers/{container_id}/files/{file_id}/content",
        "/skills": "/api/v1/skills",
        "/skills/{skill_id}": "/api/v1/skills/{skill_id}",
        "/skills/{skill_id}/content": "/api/v1/skills/{skill_id}/content",
        "/skills/{skill_id}/versions": "/api/v1/skills/{skill_id}/versions",
        "/skills/{skill_id}/versions/{version}": "/api/v1/skills/{skill_id}/versions/{version}",
        "/skills/{skill_id}/versions/{version}/content": "/api/v1/skills/{skill_id}/versions/{version}/content",
    }

    # Endpoints out of scope for Hadrian
    OUT_OF_SCOPE_PREFIXES = [
        "/assistants",
        "/threads",
        "/fine_tuning",
        "/batches",
        "/organization",
        "/realtime",
        "/evals",
        "/uploads",
        "/audit",
        "/invites",
        "/users",
        "/projects",  # OpenAI projects, not Hadrian projects
        "/conversations",  # OpenAI conversation API
        "/videos",  # OpenAI video generation
        "/chatkit",  # OpenAI chatkit feature
        "/audio/voice_consents",  # OpenAI voice consent management
        "/audio/voices",  # OpenAI custom voices
    ]

    # More specific out-of-scope paths (for sub-paths that don't match prefixes)
    OUT_OF_SCOPE_PATHS = [
        "/chat/completions/{completion_id}",  # OpenAI stored completions management
        "/chat/completions/{completion_id}/messages",
        "/models/{model}",  # Gateway aggregates models dynamically; DELETE is for fine-tuned models (not supported)
        "/models/{model}/permissions",  # OpenAI model permissions
        "/moderations",  # OpenAI content moderation API
        "/vector_stores/{vector_store_id}/file_batches",  # OpenAI file batch operations
        "/vector_stores/{vector_store_id}/file_batches/{batch_id}",
        "/vector_stores/{vector_store_id}/file_batches/{batch_id}/cancel",
        "/vector_stores/{vector_store_id}/file_batches/{batch_id}/files",
        "/vector_stores/{vector_store_id}/files/{file_id}/content",  # OpenAI file content
        "/responses/input_tokens",  # OpenAI-specific token endpoints
        "/responses/{response_id}/input_items",  # Not implemented (lookup returns the whole response)
    ]

    # Methods to exclude for specific paths
    OUT_OF_SCOPE_METHODS = {
        "/chat/completions": ["get"],  # GET lists stored completions - OpenAI-specific
        "/vector_stores/{vector_store_id}/files/{file_id}": ["post"],  # Update file metadata - not implemented
    }

    def __init__(
        self,
        openai_spec: dict[str, Any],
        hadrian_spec: dict[str, Any],
        verbose: bool = False,
    ):
        self.openai_spec = openai_spec
        self.hadrian_spec = hadrian_spec
        self.verbose = verbose
        self.openai_resolver = OpenAPIResolver(openai_spec)
        self.hadrian_resolver = OpenAPIResolver(hadrian_spec)

    def check_conformance(self, endpoint_filter: str | None = None) -> ConformanceReport:
        """Run full conformance check."""
        openai_paths = self.openai_spec.get("paths", {})
        hadrian_paths = self.hadrian_spec.get("paths", {})

        report = ConformanceReport(
            openai_version=self.openai_spec.get("info", {}).get("version", "unknown"),
            hadrian_version=self.hadrian_spec.get("info", {}).get("version", "unknown"),
            total_openai_endpoints=len(openai_paths),
            total_hadrian_endpoints=len(hadrian_paths),
            endpoints_checked=0,
            fully_conformant=0,
        )

        # Check each OpenAI endpoint
        for openai_path, openai_methods in openai_paths.items():
            # Skip out-of-scope endpoints
            if self._is_out_of_scope(openai_path):
                report.out_of_scope_endpoints.append(openai_path)
                continue

            # Apply endpoint filter if provided
            if endpoint_filter and endpoint_filter not in openai_path:
                continue

            hadrian_path = self.PATH_MAPPING.get(openai_path)
            if not hadrian_path:
                # Try to find with /api/v1 prefix
                hadrian_path = f"/api/v1{openai_path}"

            hadrian_methods = hadrian_paths.get(hadrian_path, {})

            for method in ["get", "post", "put", "patch", "delete"]:
                if method not in openai_methods:
                    continue

                # Skip out-of-scope methods for specific paths
                if self._is_method_out_of_scope(openai_path, method):
                    continue

                report.endpoints_checked += 1

                if method not in hadrian_methods:
                    # Endpoint missing in Hadrian - this is a violation
                    diff = EndpointDiff(
                        path=openai_path,
                        method=method.upper(),
                        missing_in_hadrian=True,
                    )
                    report.endpoints_with_diffs.append(diff)
                    report.violations.append(Violation(
                        violation_type="missing_endpoint",
                        path=openai_path,
                        method=method.upper(),
                        message=f"Endpoint {method.upper()} {openai_path} is not implemented in Hadrian",
                    ))
                    continue

                # Compare the endpoint
                endpoint_diff, violations = self._compare_endpoint(
                    openai_path,
                    method,
                    openai_methods[method],
                    hadrian_methods[method],
                )
                report.violations.extend(violations)

                if endpoint_diff.request_diffs or endpoint_diff.response_diffs or endpoint_diff.param_diffs:
                    report.endpoints_with_diffs.append(endpoint_diff)
                else:
                    report.fully_conformant += 1

        # Find Hadrian-only endpoints (extensions)
        for hadrian_path in hadrian_paths:
            # Skip admin endpoints
            if hadrian_path.startswith("/admin/"):
                continue
            # Check if this maps to any OpenAI endpoint
            is_mapped = False
            for openai_path, mapped_hadrian in self.PATH_MAPPING.items():
                if hadrian_path == mapped_hadrian:
                    is_mapped = True
                    break
            if not is_mapped:
                # Check if it follows the pattern /api/v1/...
                if hadrian_path.startswith("/api/v1/"):
                    possible_openai = hadrian_path.replace("/api/v1/", "/")
                    if possible_openai not in openai_paths:
                        report.hadrian_only_endpoints.append(hadrian_path)

        return report

    def _is_out_of_scope(self, path: str) -> bool:
        """Check if an OpenAI path is out of scope for Hadrian."""
        # Check explicit paths first
        if path in self.OUT_OF_SCOPE_PATHS:
            return True
        # Check path patterns (handle path params)
        for out_of_scope_path in self.OUT_OF_SCOPE_PATHS:
            if self._path_matches_pattern(path, out_of_scope_path):
                return True
        # Check prefixes
        for prefix in self.OUT_OF_SCOPE_PREFIXES:
            if path.startswith(prefix):
                return True
        return False

    def _path_matches_pattern(self, path: str, pattern: str) -> bool:
        """Check if a path matches a pattern with path parameters."""
        path_parts = path.split("/")
        pattern_parts = pattern.split("/")
        if len(path_parts) != len(pattern_parts):
            return False
        for path_part, pattern_part in zip(path_parts, pattern_parts):
            if pattern_part.startswith("{") and pattern_part.endswith("}"):
                continue  # Path parameter, matches anything
            if path_part != pattern_part:
                return False
        return True

    def _is_method_out_of_scope(self, path: str, method: str) -> bool:
        """Check if a specific method is out of scope for a path."""
        # Check exact path match
        if path in self.OUT_OF_SCOPE_METHODS:
            return method in self.OUT_OF_SCOPE_METHODS[path]
        # Check path patterns
        for pattern, methods in self.OUT_OF_SCOPE_METHODS.items():
            if self._path_matches_pattern(path, pattern) and method in methods:
                return True
        return False

    def _compare_endpoint(
        self,
        path: str,
        method: str,
        openai_op: dict,
        hadrian_op: dict,
    ) -> tuple[EndpointDiff, list[Violation]]:
        """Compare a single endpoint between specs."""
        diff = EndpointDiff(path=path, method=method.upper())
        violations: list[Violation] = []
        method_upper = method.upper()

        # Compare request body
        openai_body = openai_op.get("requestBody", {}).get("content", {}).get("application/json", {}).get("schema", {})
        hadrian_body = hadrian_op.get("requestBody", {}).get("content", {}).get("application/json", {}).get("schema", {})

        if openai_body and hadrian_body:
            resolved_openai = self.openai_resolver.resolve_schema(openai_body)
            resolved_hadrian = self.hadrian_resolver.resolve_schema(hadrian_body)
            diff.request_diffs, req_violations = self._compare_schemas(
                resolved_openai,
                resolved_hadrian,
                f"{path} request",
                path,
                method_upper,
                "request",
            )
            violations.extend(req_violations)

        # Compare response body (200 response)
        openai_resp = openai_op.get("responses", {}).get("200", {}).get("content", {}).get("application/json", {}).get("schema", {})
        hadrian_resp = hadrian_op.get("responses", {}).get("200", {}).get("content", {}).get("application/json", {}).get("schema", {})

        if openai_resp and hadrian_resp:
            resolved_openai = self.openai_resolver.resolve_schema(openai_resp)
            resolved_hadrian = self.hadrian_resolver.resolve_schema(hadrian_resp)
            diff.response_diffs, resp_violations = self._compare_schemas(
                resolved_openai,
                resolved_hadrian,
                f"{path} response",
                path,
                method_upper,
                "response",
            )
            violations.extend(resp_violations)

        # Compare query parameters
        openai_params = {p.get("name"): p for p in openai_op.get("parameters", []) if p.get("in") == "query"}
        hadrian_params = {p.get("name"): p for p in hadrian_op.get("parameters", []) if p.get("in") == "query"}

        for param_name, openai_param in openai_params.items():
            if param_name not in hadrian_params:
                diff.param_diffs.append(SchemaDiff(
                    path=f"{path} params",
                    field=param_name,
                    diff_type=DiffType.MISSING_IN_HADRIAN,
                    openai_value=openai_param.get("schema", {}).get("type"),
                    description=f"Query parameter '{param_name}' missing in Hadrian",
                ))
                # Check if documented
                key = (path, method_upper, "param", param_name)
                if key not in DOCUMENTED_MISSING_FIELDS:
                    violations.append(Violation(
                        violation_type="undocumented_missing",
                        path=path,
                        method=method_upper,
                        field=param_name,
                        location="param",
                        message=f"Query parameter '{param_name}' is missing but not documented in DOCUMENTED_MISSING_FIELDS",
                    ))

        for param_name, hadrian_param in hadrian_params.items():
            if param_name not in openai_params:
                diff.param_diffs.append(SchemaDiff(
                    path=f"{path} params",
                    field=param_name,
                    diff_type=DiffType.HADRIAN_EXTENSION,
                    hadrian_value=hadrian_param.get("schema", {}).get("type"),
                    description=f"Query parameter '{param_name}' is a Hadrian extension",
                ))
                # Check if marked as extension in description
                description = hadrian_param.get("description", "")
                if EXTENSION_MARKER not in description:
                    violations.append(Violation(
                        violation_type="unmarked_extension",
                        path=path,
                        method=method_upper,
                        field=param_name,
                        location="param",
                        message=f"Query parameter '{param_name}' is a Hadrian extension but missing '{EXTENSION_MARKER}' in description",
                    ))

        return diff, violations

    def _compare_schemas(
        self,
        openai_schema: dict,
        hadrian_schema: dict,
        context: str,
        endpoint_path: str,
        method: str,
        location: str,
    ) -> tuple[list[SchemaDiff], list[Violation]]:
        """Compare two resolved schemas."""
        diffs: list[SchemaDiff] = []
        violations: list[Violation] = []

        openai_props = openai_schema.get("properties", {})
        hadrian_props = hadrian_schema.get("properties", {})

        openai_required = set(openai_schema.get("required", []))
        hadrian_required = set(hadrian_schema.get("required", []))

        # Fields in OpenAI but not Hadrian
        for field_name, openai_field in openai_props.items():
            if field_name not in hadrian_props:
                field_type = self._get_type_string(openai_field)
                is_required = field_name in openai_required
                diffs.append(SchemaDiff(
                    path=context,
                    field=field_name,
                    diff_type=DiffType.MISSING_IN_HADRIAN,
                    openai_value=field_type,
                    description=f"Field '{field_name}' ({field_type}) missing in Hadrian" + (" [REQUIRED]" if is_required else ""),
                ))
                # Check if documented
                key = (endpoint_path, method, location, field_name)
                if key not in DOCUMENTED_MISSING_FIELDS:
                    violations.append(Violation(
                        violation_type="undocumented_missing",
                        path=endpoint_path,
                        method=method,
                        field=field_name,
                        location=location,
                        message=f"Field '{field_name}' is missing but not documented in DOCUMENTED_MISSING_FIELDS",
                    ))

        # Fields in Hadrian but not OpenAI
        for field_name, hadrian_field in hadrian_props.items():
            if field_name not in openai_props:
                field_type = self._get_type_string(hadrian_field)
                diffs.append(SchemaDiff(
                    path=context,
                    field=field_name,
                    diff_type=DiffType.HADRIAN_EXTENSION,
                    hadrian_value=field_type,
                    description=f"Field '{field_name}' ({field_type}) is a Hadrian extension",
                ))
                # Check if marked as extension in description
                description = hadrian_field.get("description", "")
                if EXTENSION_MARKER not in description:
                    violations.append(Violation(
                        violation_type="unmarked_extension",
                        path=endpoint_path,
                        method=method,
                        field=field_name,
                        location=location,
                        message=f"Field '{field_name}' is a Hadrian extension but missing '{EXTENSION_MARKER}' in description",
                    ))

        # Compare common fields
        for field_name in set(openai_props.keys()) & set(hadrian_props.keys()):
            openai_field = openai_props[field_name]
            hadrian_field = hadrian_props[field_name]

            # Check type mismatch
            openai_type = self._get_type_string(openai_field)
            hadrian_type = self._get_type_string(hadrian_field)

            if not self._types_compatible(openai_type, hadrian_type):
                diffs.append(SchemaDiff(
                    path=context,
                    field=field_name,
                    diff_type=DiffType.TYPE_MISMATCH,
                    openai_value=openai_type,
                    hadrian_value=hadrian_type,
                    description=f"Type mismatch for '{field_name}': OpenAI={openai_type}, Hadrian={hadrian_type}",
                ))

            # Check required mismatch
            openai_req = field_name in openai_required
            hadrian_req = field_name in hadrian_required
            if openai_req and not hadrian_req:
                diffs.append(SchemaDiff(
                    path=context,
                    field=field_name,
                    diff_type=DiffType.REQUIRED_MISMATCH,
                    openai_value="required",
                    hadrian_value="optional",
                    description=f"Field '{field_name}' is required in OpenAI but optional in Hadrian",
                ))

            # Recursively compare nested objects
            if openai_field.get("type") == "object" and hadrian_field.get("type") == "object":
                nested_diffs, nested_violations = self._compare_schemas(
                    openai_field,
                    hadrian_field,
                    f"{context}.{field_name}",
                    endpoint_path,
                    method,
                    location,
                )
                diffs.extend(nested_diffs)
                violations.extend(nested_violations)

            # Recurse into discriminated array<oneOf> shapes (e.g.
            # `tools[]`, `input[]`, `output[]`). Each known `type`
            # value is compared against its counterpart so
            # variant-level differences (e.g. `tools[mcp].connector_id`)
            # surface in the report. Handles OpenAPI 3.1's nullable
            # form `type: ["array", "null"]` on either side.
            if _is_array_type(openai_field) and _is_array_type(hadrian_field):
                u_diffs, u_violations = self._compare_union_items(
                    openai_field.get("items", {}),
                    hadrian_field.get("items", {}),
                    f"{context}.{field_name}[]",
                    endpoint_path,
                    method,
                    location,
                )
                diffs.extend(u_diffs)
                violations.extend(u_violations)

        return diffs, violations

    def _compare_union_items(
        self,
        openai_items: dict,
        hadrian_items: dict,
        context: str,
        endpoint_path: str,
        method: str,
        location: str,
    ) -> tuple[list[SchemaDiff], list[Violation]]:
        """Compare two discriminated-union `items` schemas.

        For each `type` literal present on either side, run
        `_compare_schemas` on the matching variant pair. Variants only
        on one side produce a single diff entry (missing or extension);
        we do not recurse into them.
        """
        diffs: list[SchemaDiff] = []
        violations: list[Violation] = []

        openai_variants = openai_items.get("_union_variants") if isinstance(openai_items, dict) else None
        hadrian_variants = hadrian_items.get("_union_variants") if isinstance(hadrian_items, dict) else None
        if not openai_variants or not hadrian_variants:
            return diffs, violations

        # Strip the leading "<path> request"/" response" prefix the
        # caller threaded through `context`, so the variant key matches
        # the shape used in `DOCUMENTED_MISSING_FIELDS` (e.g.
        # `tools[mcp]`, not `/responses request.tools[]<mcp>`).
        short_context = context
        for prefix in (f"{endpoint_path} request.", f"{endpoint_path} response."):
            if short_context.startswith(prefix):
                short_context = short_context[len(prefix):]
                break
        if short_context.endswith("[]"):
            short_context = short_context[:-2]

        all_type_values = set(openai_variants.keys()) | set(hadrian_variants.keys())
        for type_value in sorted(all_type_values):
            openai_variant = openai_variants.get(type_value)
            hadrian_variant = hadrian_variants.get(type_value)
            variant_key = f"{short_context}[{type_value}]"
            variant_context = f"{context}<type={type_value}>"

            if openai_variant is None:
                diffs.append(SchemaDiff(
                    path=context,
                    field=type_value,
                    diff_type=DiffType.HADRIAN_EXTENSION,
                    hadrian_value="object",
                    description=f"Variant '{type_value}' is a Hadrian extension",
                ))
                continue
            if hadrian_variant is None:
                diffs.append(SchemaDiff(
                    path=context,
                    field=type_value,
                    diff_type=DiffType.MISSING_IN_HADRIAN,
                    openai_value="object",
                    description=f"Variant '{type_value}' missing in Hadrian",
                ))
                key = (endpoint_path, method, location, variant_key)
                if key not in DOCUMENTED_MISSING_FIELDS:
                    violations.append(Violation(
                        violation_type="undocumented_missing",
                        path=endpoint_path,
                        method=method,
                        field=variant_key,
                        location=location,
                        message=f"Union variant '{type_value}' at {short_context} missing in Hadrian",
                    ))
                continue

            v_diffs, v_violations = self._compare_schemas(
                openai_variant,
                hadrian_variant,
                variant_context,
                endpoint_path,
                method,
                location,
            )
            # Tag each within-variant diff with its discriminator so
            # `cache_control on file_search` reads as such in the
            # report instead of an undifferentiated "Field 'cache_control'".
            for d in v_diffs:
                d.description = f"[{type_value}] {d.description}"
            diffs.extend(v_diffs)
            violations.extend(v_violations)

        return diffs, violations

    def _get_type_string(self, schema: dict) -> str:
        """Get a human-readable type string from a schema."""
        if not isinstance(schema, dict):
            return str(schema)

        schema_type = schema.get("type")

        # Handle array types in OpenAPI 3.1 format: ["string", "null"]
        if isinstance(schema_type, list):
            types = [t for t in schema_type if t != "null"]
            if len(types) == 1:
                schema_type = types[0]
            else:
                schema_type = "|".join(types)

        if schema_type == "array":
            items = schema.get("items", {})
            item_type = self._get_type_string(items)
            return f"array<{item_type}>"
        elif schema_type == "object":
            if "additionalProperties" in schema:
                value_type = self._get_type_string(schema["additionalProperties"])
                return f"map<string, {value_type}>"
            return "object"
        elif schema_type:
            return str(schema_type)
        elif "enum" in schema:
            return "enum"
        elif "$ref" in schema:
            ref = schema["$ref"]
            return ref.split("/")[-1]
        else:
            return "unknown"

    def _types_compatible(self, openai_type: str, hadrian_type: str) -> bool:
        """Check if two types are compatible."""
        # Exact match
        if openai_type == hadrian_type:
            return True

        # integer/number compatibility
        if {openai_type, hadrian_type} <= {"integer", "number"}:
            return True

        # Double is compatible with number
        if openai_type == "number" and hadrian_type == "double":
            return True
        if hadrian_type == "number" and openai_type == "double":
            return True

        return False


def format_text_report(report: ConformanceReport) -> str:
    """Format report as human-readable text."""
    lines = []
    lines.append("=" * 70)
    lines.append("OpenAPI Conformance Report")
    lines.append("=" * 70)
    lines.append(f"OpenAI spec version: {report.openai_version}")
    lines.append(f"Hadrian spec version: {report.hadrian_version}")
    lines.append("")
    lines.append("Summary:")
    lines.append(f"  - OpenAI endpoints: {report.total_openai_endpoints}")
    lines.append(f"  - Hadrian endpoints: {report.total_hadrian_endpoints}")
    lines.append(f"  - Endpoints checked: {report.endpoints_checked}")
    lines.append(f"  - Fully conformant: {report.fully_conformant}")
    lines.append(f"  - With differences: {len(report.endpoints_with_diffs)}")
    lines.append(f"  - Out of scope: {len(report.out_of_scope_endpoints)}")
    lines.append(f"  - Hadrian-only (extensions): {len(report.hadrian_only_endpoints)}")
    lines.append("")

    if report.endpoints_with_diffs:
        lines.append("-" * 70)
        lines.append("Endpoints with Differences:")
        lines.append("-" * 70)

        for diff in report.endpoints_with_diffs:
            lines.append(f"\n{diff.method} {diff.path}")

            if diff.missing_in_hadrian:
                lines.append("  [MISSING] Endpoint not implemented in Hadrian")
                continue

            if diff.request_diffs:
                lines.append("  Request body differences:")
                for schema_diff in diff.request_diffs:
                    icon = "[-]" if schema_diff.diff_type == DiffType.MISSING_IN_HADRIAN else "[+]"
                    if schema_diff.diff_type == DiffType.TYPE_MISMATCH:
                        icon = "[~]"
                    elif schema_diff.diff_type == DiffType.REQUIRED_MISMATCH:
                        icon = "[!]"
                    lines.append(f"    {icon} {schema_diff.description}")

            if diff.response_diffs:
                lines.append("  Response body differences:")
                for schema_diff in diff.response_diffs:
                    icon = "[-]" if schema_diff.diff_type == DiffType.MISSING_IN_HADRIAN else "[+]"
                    if schema_diff.diff_type == DiffType.TYPE_MISMATCH:
                        icon = "[~]"
                    elif schema_diff.diff_type == DiffType.REQUIRED_MISMATCH:
                        icon = "[!]"
                    lines.append(f"    {icon} {schema_diff.description}")

            if diff.param_diffs:
                lines.append("  Query parameter differences:")
                for schema_diff in diff.param_diffs:
                    icon = "[-]" if schema_diff.diff_type == DiffType.MISSING_IN_HADRIAN else "[+]"
                    lines.append(f"    {icon} {schema_diff.description}")

    if report.hadrian_only_endpoints:
        lines.append("")
        lines.append("-" * 70)
        lines.append("Hadrian Extension Endpoints:")
        lines.append("-" * 70)
        for path in report.hadrian_only_endpoints:
            lines.append(f"  [+] {path}")

    # CI Violations section
    if report.violations:
        lines.append("")
        lines.append("=" * 70)
        lines.append("CI VIOLATIONS (will cause CI to fail)")
        lines.append("=" * 70)

        # Group violations by type
        missing_endpoints = [v for v in report.violations if v.violation_type == "missing_endpoint"]
        undocumented = [v for v in report.violations if v.violation_type == "undocumented_missing"]
        unmarked = [v for v in report.violations if v.violation_type == "unmarked_extension"]

        if missing_endpoints:
            lines.append("")
            lines.append("Missing Endpoints (must be implemented):")
            for v in missing_endpoints:
                lines.append(f"  - {v.method} {v.path}")

        if undocumented:
            lines.append("")
            lines.append("Undocumented Missing Fields (add to DOCUMENTED_MISSING_FIELDS):")
            for v in undocumented:
                lines.append(f"  - {v.method} {v.path} [{v.location}] {v.field}")

        if unmarked:
            lines.append("")
            lines.append(f"Unmarked Extensions (add '{EXTENSION_MARKER}' to description):")
            for v in unmarked:
                lines.append(f"  - {v.method} {v.path} [{v.location}] {v.field}")

        lines.append("")
        lines.append(f"Total violations: {len(report.violations)}")
    else:
        lines.append("")
        lines.append("=" * 70)
        lines.append("CI STATUS: PASS (no violations)")
        lines.append("=" * 70)

    lines.append("")
    lines.append("Legend:")
    lines.append("  [-] Missing in Hadrian")
    lines.append("  [+] Hadrian extension")
    lines.append("  [~] Type mismatch")
    lines.append("  [!] Required/optional mismatch")

    return "\n".join(lines)


def format_json_report(report: ConformanceReport) -> str:
    """Format report as JSON."""
    def diff_to_dict(diff: SchemaDiff) -> dict:
        return {
            "path": diff.path,
            "field": diff.field,
            "type": diff.diff_type.value,
            "openai_value": diff.openai_value,
            "hadrian_value": diff.hadrian_value,
            "description": diff.description,
        }

    def endpoint_to_dict(diff: EndpointDiff) -> dict:
        return {
            "path": diff.path,
            "method": diff.method,
            "missing_in_hadrian": diff.missing_in_hadrian,
            "hadrian_extension": diff.hadrian_extension,
            "request_diffs": [diff_to_dict(d) for d in diff.request_diffs],
            "response_diffs": [diff_to_dict(d) for d in diff.response_diffs],
            "param_diffs": [diff_to_dict(d) for d in diff.param_diffs],
        }

    def violation_to_dict(v: Violation) -> dict:
        return {
            "type": v.violation_type,
            "path": v.path,
            "method": v.method,
            "field": v.field,
            "location": v.location,
            "message": v.message,
        }

    data = {
        "openai_version": report.openai_version,
        "hadrian_version": report.hadrian_version,
        "summary": {
            "total_openai_endpoints": report.total_openai_endpoints,
            "total_hadrian_endpoints": report.total_hadrian_endpoints,
            "endpoints_checked": report.endpoints_checked,
            "fully_conformant": report.fully_conformant,
            "with_differences": len(report.endpoints_with_diffs),
            "out_of_scope": len(report.out_of_scope_endpoints),
            "hadrian_extensions": len(report.hadrian_only_endpoints),
            "violations": len(report.violations),
        },
        "violations": [violation_to_dict(v) for v in report.violations],
        "endpoints_with_diffs": [endpoint_to_dict(d) for d in report.endpoints_with_diffs],
        "hadrian_only_endpoints": report.hadrian_only_endpoints,
        "out_of_scope_endpoints": report.out_of_scope_endpoints,
    }

    return json.dumps(data, indent=2)


def main():
    parser = argparse.ArgumentParser(
        description="Check Hadrian OpenAPI spec conformance against OpenAI spec"
    )
    parser.add_argument(
        "--format",
        choices=["text", "json"],
        default="text",
        help="Output format (default: text)",
    )
    parser.add_argument(
        "--endpoint",
        type=str,
        help="Filter to specific endpoint (e.g., /chat/completions)",
    )
    parser.add_argument(
        "--verbose",
        "-v",
        action="store_true",
        help="Show verbose output",
    )
    parser.add_argument(
        "--openai-spec",
        type=Path,
        default=Path("openapi/openai.openapi.json"),
        help="Path to OpenAI spec (default: openapi/openai.openapi.json)",
    )
    parser.add_argument(
        "--hadrian-spec",
        type=Path,
        default=Path("openapi/hadrian.openapi.json"),
        help="Path to Hadrian spec (default: openapi/hadrian.openapi.json)",
    )

    args = parser.parse_args()

    # Find project root (where openapi/ directory is)
    script_dir = Path(__file__).parent
    project_root = script_dir.parent

    openai_path = project_root / args.openai_spec
    hadrian_path = project_root / args.hadrian_spec

    if not openai_path.exists():
        print(f"Error: OpenAI spec not found at {openai_path}", file=sys.stderr)
        print("Run ./scripts/fetch-openapi-specs.sh openai to download it.", file=sys.stderr)
        sys.exit(1)

    if not hadrian_path.exists():
        print(f"Error: Hadrian spec not found at {hadrian_path}", file=sys.stderr)
        sys.exit(1)

    # Load specs
    with open(openai_path) as f:
        openai_spec = json.load(f)

    with open(hadrian_path) as f:
        hadrian_spec = json.load(f)

    # Run conformance check
    checker = ConformanceChecker(openai_spec, hadrian_spec, verbose=args.verbose)
    report = checker.check_conformance(endpoint_filter=args.endpoint)

    # Output report
    if args.format == "json":
        print(format_json_report(report))
    else:
        print(format_text_report(report))

    # Exit with non-zero if there are any violations
    if report.violations:
        sys.exit(1)


if __name__ == "__main__":
    main()
