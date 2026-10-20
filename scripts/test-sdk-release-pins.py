#!/usr/bin/env python3
"""Keep TinyMCP's module version and released TinyBus SDK pin aligned."""

from __future__ import annotations

import subprocess
import tomllib
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SDK_URL = "https://github.com/tinyhumansai/tinybus"
SDK_VERSION = "0.3.3"
SDK_REV = "207a9a7df15e44fdba00a807d0f7eaf21904ba0e"
MODULE_VERSION = "0.20.0"


def manifest(path: Path) -> dict:
    return tomllib.loads(path.read_text())


class SdkReleasePinTests(unittest.TestCase):
    def test_module_and_bus_contract_share_the_next_release_version(self) -> None:
        workspace = manifest(ROOT / "Cargo.toml")
        self.assertEqual(workspace["workspace"]["package"]["version"], MODULE_VERSION)
        for path in (
            ROOT / "crates/tinymcp/Cargo.toml",
            ROOT / "crates/tinymcp-bus/Cargo.toml",
        ):
            with self.subTest(path=path.relative_to(ROOT)):
                self.assertEqual(manifest(path)["package"]["version"], {"workspace": True})

    def test_module_sdk_dependencies_share_the_published_commit(self) -> None:
        dependencies = manifest(ROOT / "Cargo.toml")["workspace"]["dependencies"]
        for name in ("tinybus", "tinybus-module"):
            with self.subTest(name=name):
                dependency = dependencies[name]
                self.assertEqual(dependency["version"], SDK_VERSION)
                self.assertEqual(dependency["git"], SDK_URL)
                self.assertEqual(dependency["rev"], SDK_REV)

    def test_vendor_gitlink_is_pinned_to_the_exact_sdk_commit(self) -> None:
        tree = subprocess.run(
            ["git", "ls-tree", "HEAD", "vendor/tinybus"],
            cwd=ROOT,
            check=True,
            text=True,
            capture_output=True,
        ).stdout.split()
        self.assertEqual(tree[2:], [SDK_REV, "vendor/tinybus"])

    def test_pure_bus_consumer_does_not_link_the_module_sdk(self) -> None:
        consumer = manifest(ROOT / "crates/tinymcp-bus/consumer/Cargo.toml")
        dependencies = consumer.get("dependencies", {})
        self.assertNotIn("tinybus", dependencies)
        self.assertNotIn("tinybus-module", dependencies)


if __name__ == "__main__":
    unittest.main()
