#!/usr/bin/env -S uv run --with pytest pytest -q
# /// script
# requires-python = ">=3.12"
# dependencies = [
#     "pyyaml>=6.0",
#     "pytest>=8",
# ]
# ///
"""
Tests for scripts/openapi-conformance.py.

The conformance script lives next to this file. We load it as a module via
importlib so the dash in the filename doesn't trip the regular import system.

Run directly:

    ./scripts/test_openapi_conformance.py

Or via pytest if pyyaml/pytest are already on PYTHONPATH:

    pytest scripts/test_openapi_conformance.py
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location(
    "openapi_conformance",
    _HERE / "openapi-conformance.py",
)
assert _SPEC is not None and _SPEC.loader is not None
_module = importlib.util.module_from_spec(_SPEC)
# dataclass introspects sys.modules[cls.__module__]; register before exec.
sys.modules["openapi_conformance"] = _module
_SPEC.loader.exec_module(_module)

OpenAPIResolver = _module.OpenAPIResolver
ConformanceChecker = _module.ConformanceChecker
DiffType = _module.DiffType
EXTENSION_MARKER = _module.EXTENSION_MARKER


# ---------------------------------------------------------------------------
# OpenAPIResolver.resolve_ref
# ---------------------------------------------------------------------------


def test_resolve_ref_returns_target_schema():
    spec = {
        "components": {
            "schemas": {
                "Foo": {"type": "object", "properties": {"a": {"type": "string"}}},
            }
        }
    }
    resolver = OpenAPIResolver(spec)
    resolved = resolver.resolve_ref("#/components/schemas/Foo")
    assert resolved["type"] == "object"
    assert "a" in resolved["properties"]


def test_resolve_ref_caches_result():
    spec = {
        "components": {
            "schemas": {"Foo": {"type": "string"}},
        }
    }
    resolver = OpenAPIResolver(spec)
    a = resolver.resolve_ref("#/components/schemas/Foo")
    b = resolver.resolve_ref("#/components/schemas/Foo")
    assert a is b  # cached object identity


def test_resolve_ref_missing_returns_empty_dict():
    resolver = OpenAPIResolver({"components": {"schemas": {}}})
    assert resolver.resolve_ref("#/components/schemas/DoesNotExist") == {}


def test_resolve_ref_non_anchor_returns_empty_dict():
    resolver = OpenAPIResolver({})
    assert resolver.resolve_ref("https://example.com/schema") == {}


def test_resolve_ref_follows_chain():
    spec = {
        "components": {
            "schemas": {
                "Inner": {"type": "integer"},
                "Outer": {"$ref": "#/components/schemas/Inner"},
            }
        }
    }
    resolver = OpenAPIResolver(spec)
    assert resolver.resolve_ref("#/components/schemas/Outer")["type"] == "integer"


# ---------------------------------------------------------------------------
# OpenAPIResolver.resolve_schema — allOf
# ---------------------------------------------------------------------------


def test_resolve_schema_merges_allof_properties():
    spec = {
        "components": {
            "schemas": {
                "Base": {
                    "type": "object",
                    "properties": {"id": {"type": "string"}},
                    "required": ["id"],
                },
                "Extended": {
                    "allOf": [
                        {"$ref": "#/components/schemas/Base"},
                        {
                            "type": "object",
                            "properties": {"name": {"type": "string"}},
                            "required": ["name"],
                        },
                    ]
                },
            }
        }
    }
    resolver = OpenAPIResolver(spec)
    resolved = resolver.resolve_ref("#/components/schemas/Extended")
    assert set(resolved["properties"].keys()) == {"id", "name"}
    assert set(resolved["required"]) == {"id", "name"}


def test_resolve_schema_allof_overlapping_required_not_duplicated():
    spec: dict = {"components": {"schemas": {}}}
    resolver = OpenAPIResolver(spec)
    resolved = resolver.resolve_schema(
        {
            "allOf": [
                {"properties": {"x": {"type": "string"}}, "required": ["x"]},
                {"properties": {"x": {"type": "string"}}, "required": ["x"]},
            ]
        }
    )
    assert resolved["required"] == ["x"]


# ---------------------------------------------------------------------------
# OpenAPIResolver.resolve_schema — oneOf / anyOf
# ---------------------------------------------------------------------------


def test_resolve_schema_oneof_picks_first_non_null_and_marks_nullable():
    spec: dict = {"components": {"schemas": {}}}
    resolver = OpenAPIResolver(spec)
    resolved = resolver.resolve_schema(
        {
            "oneOf": [
                {"type": "null"},
                {"type": "string"},
                {"type": "integer"},
            ]
        }
    )
    assert resolved["type"] == "string"
    assert resolved["_nullable"] is True


def test_resolve_schema_anyof_only_null_returns_null_type():
    resolver = OpenAPIResolver({"components": {"schemas": {}}})
    resolved = resolver.resolve_schema({"anyOf": [{"type": "null"}]})
    assert resolved["type"] == "null"
    assert resolved["_nullable"] is True


def test_resolve_schema_anyof_uses_same_logic_as_oneof():
    resolver = OpenAPIResolver({"components": {"schemas": {}}})
    resolved = resolver.resolve_schema(
        {"anyOf": [{"type": "boolean"}, {"type": "null"}]}
    )
    assert resolved["type"] == "boolean"
    assert resolved["_nullable"] is True


# ---------------------------------------------------------------------------
# OpenAPIResolver.resolve_schema — passthrough fields & nested resolution
# ---------------------------------------------------------------------------


def test_resolve_schema_resolves_nested_properties():
    spec = {
        "components": {
            "schemas": {
                "Inner": {"type": "integer"},
            }
        }
    }
    resolver = OpenAPIResolver(spec)
    resolved = resolver.resolve_schema(
        {
            "type": "object",
            "properties": {
                "n": {"$ref": "#/components/schemas/Inner"},
            },
        }
    )
    assert resolved["properties"]["n"]["type"] == "integer"


def test_resolve_schema_resolves_array_items_ref():
    spec = {"components": {"schemas": {"Item": {"type": "string"}}}}
    resolver = OpenAPIResolver(spec)
    resolved = resolver.resolve_schema(
        {"type": "array", "items": {"$ref": "#/components/schemas/Item"}}
    )
    assert resolved["items"]["type"] == "string"


def test_resolve_schema_ref_overrides_keep_local_keys():
    """A $ref alongside other keys (like description) should keep the local keys."""
    spec = {"components": {"schemas": {"Inner": {"type": "string"}}}}
    resolver = OpenAPIResolver(spec)
    resolved = resolver.resolve_schema(
        {"$ref": "#/components/schemas/Inner", "description": "local override"}
    )
    assert resolved["type"] == "string"
    assert resolved["description"] == "local override"


# ---------------------------------------------------------------------------
# ConformanceChecker — small end-to-end sanity check
# ---------------------------------------------------------------------------


def _minimal_spec(paths: dict, version: str = "1.0.0") -> dict:
    return {
        "info": {"version": version, "title": "test"},
        "paths": paths,
        "components": {"schemas": {}},
    }


def test_conformance_flags_missing_endpoint_as_violation():
    openai_spec = _minimal_spec(
        {
            "/embeddings": {
                "post": {
                    "requestBody": {
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "properties": {"input": {"type": "string"}},
                                    "required": ["input"],
                                }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "content": {
                                "application/json": {
                                    "schema": {"type": "object"}
                                }
                            }
                        }
                    },
                }
            }
        }
    )
    hadrian_spec = _minimal_spec({})  # nothing implemented

    report = ConformanceChecker(openai_spec, hadrian_spec).check_conformance()

    assert report.endpoints_checked == 1
    assert report.fully_conformant == 0
    assert any(
        v.violation_type == "missing_endpoint" and v.path == "/embeddings"
        for v in report.violations
    )


def test_conformance_skips_beta_query_path_keys():
    # OpenAI publishes beta endpoint variants as path keys with a literal query
    # string (e.g. "/responses?beta=true"); these must not count as missing.
    openai_spec = _minimal_spec(
        {
            "/responses?beta=true": {
                "post": {
                    "responses": {
                        "200": {
                            "content": {
                                "application/json": {
                                    "schema": {"type": "object"}
                                }
                            }
                        }
                    },
                }
            }
        }
    )
    hadrian_spec = _minimal_spec({})  # nothing implemented

    report = ConformanceChecker(openai_spec, hadrian_spec).check_conformance()

    assert report.endpoints_checked == 0
    assert not report.violations
    assert "/responses?beta=true" in report.out_of_scope_endpoints


def test_conformance_passes_for_matching_endpoint():
    schema = {
        "type": "object",
        "properties": {"input": {"type": "string"}},
        "required": ["input"],
    }
    openai_spec = _minimal_spec(
        {
            "/embeddings": {
                "post": {
                    "requestBody": {
                        "content": {"application/json": {"schema": schema}}
                    },
                    "responses": {
                        "200": {
                            "content": {
                                "application/json": {
                                    "schema": {"type": "object"}
                                }
                            }
                        }
                    },
                }
            }
        }
    )
    hadrian_spec = _minimal_spec(
        {
            "/api/v1/embeddings": {
                "post": {
                    "requestBody": {
                        "content": {"application/json": {"schema": schema}}
                    },
                    "responses": {
                        "200": {
                            "content": {
                                "application/json": {
                                    "schema": {"type": "object"}
                                }
                            }
                        }
                    },
                }
            }
        }
    )

    report = ConformanceChecker(openai_spec, hadrian_spec).check_conformance()

    missing_endpoint_violations = [
        v for v in report.violations if v.violation_type == "missing_endpoint"
    ]
    assert missing_endpoint_violations == []


def test_extension_marker_constant_is_stable():
    # Tests asserting against extension docs use this marker; if it changes,
    # callers must update too.
    assert EXTENSION_MARKER == "**Hadrian Extension:**"


def test_diff_type_enum_values():
    # Enum values are part of the JSON report contract (consumed by CI and dashboards).
    assert DiffType.MISSING_IN_HADRIAN.value == "missing_in_hadrian"
    assert DiffType.HADRIAN_EXTENSION.value == "hadrian_extension"
    assert DiffType.TYPE_MISMATCH.value == "type_mismatch"
    assert DiffType.REQUIRED_MISMATCH.value == "required_mismatch"


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-q"]))
