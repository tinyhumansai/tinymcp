#!/usr/bin/env python3
"""Static regression guard for the TinyBus contract ownership boundary."""

from __future__ import annotations

import unittest
from pathlib import Path
import tomllib

ROOT = Path(__file__).resolve().parents[1]
BUS = ROOT / "crates/tinymcp-bus/src"
MODULE = ROOT / "crates/tinymcp/src/storage"
CLIENT = ROOT / "crates/tinymcp-client/src"


def manifest(path: Path) -> dict:
    return tomllib.loads(path.read_text())


class PureBusOwnershipTests(unittest.TestCase):
    def test_bus_keeps_storage_needs_but_no_schema_execution(self) -> None:
        storage = (BUS / "storage/mod.rs").read_text()
        self.assertIn("pub fn storage_needs()", storage)
        self.assertNotIn("storage_schema", storage)
        self.assertNotIn("tinystoragedrivers_core", storage)
        self.assertFalse((BUS / "storage/schema.rs").exists())
        self.assertFalse((BUS / "storage/migrate.rs").exists())

    def test_storage_schema_and_migrations_are_module_owned(self) -> None:
        self.assertTrue((MODULE / "schema.rs").exists())
        self.assertTrue((MODULE / "migrate.rs").exists())
        self.assertTrue((MODULE / "schema_tests.rs").exists())
        self.assertTrue((MODULE / "migrate_tests.rs").exists())

    def test_client_lifecycle_implementation_has_its_own_crate(self) -> None:
        self.assertTrue((CLIENT / "lib.rs").exists())
        lifecycle = CLIENT / "client_lifecycle"
        self.assertTrue((lifecycle / "machine.rs").exists())
        self.assertTrue((lifecycle / "table.rs").exists())
        self.assertFalse((BUS / "client_lifecycle").exists())
        bus_lib = (BUS / "lib.rs").read_text()
        self.assertNotIn("pub mod client_lifecycle", bus_lib)

    def test_contract_all_features_and_client_dependencies_stay_pure(self) -> None:
        bus = manifest(ROOT / "crates/tinymcp-bus/Cargo.toml")
        dependencies = bus.get("dependencies", {})
        self.assertNotIn("tinystoragedrivers-core", dependencies)
        self.assertNotIn("tokio", dependencies)
        self.assertNotIn("tinystoragedrivers-core", bus.get("dev-dependencies", {}))
        self.assertNotIn("tokio", bus.get("dev-dependencies", {}))
        module = manifest(ROOT / "crates/tinymcp/Cargo.toml")
        self.assertIn("tinystoragedrivers-core", module["dependencies"])
        self.assertEqual(module["dependencies"]["tinymcp-bus"], {"workspace": True})
        client = manifest(ROOT / "crates/tinymcp-client/Cargo.toml")
        self.assertEqual(
            set(client.get("dependencies", {})),
            {"tinymcp-bus"},
        )
        self.assertEqual(client["package"]["name"], "tinymcp-client")
        self.assertIn("storage = []", (ROOT / "crates/tinymcp-bus/Cargo.toml").read_text())


if __name__ == "__main__":
    unittest.main()
