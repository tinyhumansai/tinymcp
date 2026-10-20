#!/usr/bin/env python3
"""Behavioral negative controls for the storage-needs dependency graph guard."""

from __future__ import annotations

import copy
import importlib.util
import sys
import unittest
from pathlib import Path

sys.dont_write_bytecode = True
GUARD_PATH = Path(__file__).with_name("check-storage-needs-consumer.py")
SPEC = importlib.util.spec_from_file_location("storage_needs_consumer_guard", GUARD_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"cannot load dependency guard at {GUARD_PATH}")
guard = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(guard)

ROOT_ID = "path+tinymcp-storage-needs-consumer#0.0.0"
CONTRACT_ID = "path+tinymcp-bus#0.20.0"
TSD_ID = (
    "git+https://github.com/tinyhumansai/tinystoragedrivers?rev="
    "c9ea6ff1615b77b55d7269403af5fd11bedc9ce8#"
    "c9ea6ff1615b77b55d7269403af5fd11bedc9ce8"
)
SERDE_ID = "registry+https://github.com/rust-lang/crates.io-index#serde@1.0.0"


def dependency(package_id: str, kind: str | None = None) -> dict:
    return {"pkg": package_id, "dep_kinds": [{"kind": kind, "target": None}]}


def fixture() -> dict:
    packages = [
        {
            "id": ROOT_ID,
            "name": "tinymcp-storage-needs-consumer",
            "version": "0.0.0",
            "manifest_path": str(guard.CONSUMER_MANIFEST),
        },
        {
            "id": CONTRACT_ID,
            "name": "tinymcp-bus",
            "version": "0.20.0",
            "manifest_path": str(guard.CONTRACT_MANIFEST),
        },
        {
            "id": TSD_ID,
            "name": "tinystoragedrivers-bus",
            "version": "1.5.0",
            "source": TSD_ID,
        },
        {
            "id": SERDE_ID,
            "name": "serde",
            "version": "1.0.0",
            "source": "registry+https://github.com/rust-lang/crates.io-index",
        },
    ]
    nodes = [
        {
            "id": ROOT_ID,
            "features": [],
            "deps": [dependency(CONTRACT_ID), dependency(TSD_ID)],
        },
        {"id": CONTRACT_ID, "features": [], "deps": [dependency(TSD_ID)]},
        {"id": TSD_ID, "features": [], "deps": [dependency(SERDE_ID)]},
        {"id": SERDE_ID, "features": ["derive"], "deps": []},
    ]
    return {"packages": packages, "resolve": {"root": ROOT_ID, "nodes": nodes}}


class StorageNeedsConsumerGuardTests(unittest.TestCase):
    def test_accepts_the_canonical_shared_contract_graph(self) -> None:
        guard.validate_metadata(fixture())

    def test_rejects_forbidden_packages_on_normal_or_build_edges(self) -> None:
        for name in (
            "tinymcp",
            "tinybus",
            "tinystoragedrivers-client",
            "tokio",
            "reqwest",
            "rusqlite",
        ):
            with self.subTest(name=name):
                metadata = fixture()
                package_id = f"registry+https://index.invalid#{name}@1.0.0"
                metadata["packages"].append(
                    {"id": package_id, "name": name, "version": "1.0.0", "source": "registry+index"}
                )
                metadata["resolve"]["nodes"].append(
                    {"id": package_id, "features": [], "deps": []}
                )
                metadata["resolve"]["nodes"][2]["deps"].append(dependency(package_id))
                with self.assertRaisesRegex(RuntimeError, "forbidden|implementation|storage implementation"):
                    guard.validate_metadata(metadata)

    def test_rejects_a_build_dependency_even_when_it_is_not_normal(self) -> None:
        metadata = fixture()
        package_id = "registry+https://index.invalid#tokio@1.0.0"
        metadata["packages"].append(
            {"id": package_id, "name": "tokio", "version": "1.0.0", "source": "registry+index"}
        )
        metadata["resolve"]["nodes"].append({"id": package_id, "features": [], "deps": []})
        metadata["resolve"]["nodes"][2]["deps"].append(dependency(package_id, "build"))
        with self.assertRaisesRegex(RuntimeError, "forbidden"):
            guard.validate_metadata(metadata)

    def test_rejects_client_feature_enabled_on_the_canonical_bus_package(self) -> None:
        metadata = fixture()
        tsd_node = next(node for node in metadata["resolve"]["nodes"] if node["id"] == TSD_ID)
        tsd_node["features"] = ["client"]
        with self.assertRaisesRegex(RuntimeError, "client feature"):
            guard.validate_metadata(metadata)

    def test_rejects_a_tag_or_other_source_even_when_the_version_matches(self) -> None:
        metadata = copy.deepcopy(fixture())
        tsd_package = next(package for package in metadata["packages"] if package["id"] == TSD_ID)
        tsd_package["source"] = "git+https://github.com/tinyhumansai/tinystoragedrivers?tag=v1.5.0#commit"
        with self.assertRaisesRegex(RuntimeError, "canonical 1.5 revision"):
            guard.validate_metadata(metadata)

    def test_ignores_dev_only_edges_because_the_guard_is_normal_and_build(self) -> None:
        metadata = fixture()
        package_id = "registry+https://index.invalid#rusqlite@1.0.0"
        metadata["packages"].append(
            {"id": package_id, "name": "rusqlite", "version": "1.0.0", "source": "registry+index"}
        )
        metadata["resolve"]["nodes"].append({"id": package_id, "features": [], "deps": []})
        metadata["resolve"]["nodes"][1]["deps"].append(dependency(package_id, "dev"))
        guard.validate_metadata(metadata)


if __name__ == "__main__":
    unittest.main()
