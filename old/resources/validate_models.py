#!/usr/bin/env python3
"""Credential-free catalog validation; keep invariants aligned with model_catalog.rs."""
import json
import re
from pathlib import Path

path = Path(__file__).with_name("models.json")
raw = path.read_bytes()
assert len(raw) <= 1024 * 1024, "catalog exceeds 1 MiB"
catalog = json.loads(raw)
assert set(catalog) == {"schema_version", "revision", "models"}
assert type(catalog["schema_version"]) is int and catalog["schema_version"] == 1
assert type(catalog["revision"]) is int and 0 < catalog["revision"] < 2**64
assert isinstance(catalog["models"], list) and catalog["models"], "empty catalog"
prefixes = {"antigravity": "antigravity/", "opencode": "opencode-go/", "anthropic": "anthropic/"}
def effort_valid(value):
    return isinstance(value, str) and re.fullmatch(r"[a-z0-9_-]{1,64}", value) is not None
seen = set()
for model in catalog["models"]:
    assert set(model) <= {"id", "label", "provider", "context_window", "supported_efforts", "default_effort"}
    mid = model["id"]
    assert isinstance(mid, str) and mid and not any(c.isspace() for c in mid)
    assert mid not in seen, f"duplicate ID: {mid}"
    seen.add(mid)
    assert isinstance(model["label"], str) and model["label"].strip()
    provider = model["provider"]
    assert provider in {"openai", *prefixes}
    if provider in prefixes:
        prefix = prefixes[provider]
        assert mid.startswith(prefix) and len(mid) > len(prefix)
    else:
        assert not mid.startswith((*prefixes.values(), "acp/"))
    context = model.get("context_window")
    assert context is None or (type(context) is int and 0 < context <= 10_000_000)
    supported = model.get("supported_efforts", [])
    assert isinstance(supported, list) and all(effort_valid(e) for e in supported)
    assert len(supported) == len(set(supported))
    default = model.get("default_effort")
    assert default is None or (effort_valid(default) and (not supported or default in supported))
print(f"Validated revision {catalog['revision']}: {len(seen)} models")
