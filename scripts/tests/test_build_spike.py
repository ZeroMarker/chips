"""Cache provenance regressions; no network or native compilation required."""
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location(
    "build_spike", Path(__file__).resolve().parents[1] / "build_spike.py")
b = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = b
SPEC.loader.exec_module(b)


class BuildCacheTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.prefix = Path(self.temporary.name) / "cache"
        self.request = {"revision": "a" * 40, "compiler": "gcc 13", "libc": ("glibc", "2.39")}

    def install_cache(self):
        binary = self.prefix / "bin/spike"
        binary.parent.mkdir(parents=True)
        binary.write_bytes(b"cached executable")
        binary.chmod(0o755)
        (self.prefix / "build.json").write_text(json.dumps({
            "request": self.request, "binary_sha256": b.digest(binary),
        }))
        return binary

    def test_absent_cache_is_a_miss_and_matching_cache_is_verified(self):
        self.assertIsNone(b.cached_binary(self.prefix, self.request))
        binary = self.install_cache()
        self.assertEqual(b.cached_binary(self.prefix, self.request), binary)

    def test_modified_binary_fails_checksum_instead_of_reusing_cache(self):
        binary = self.install_cache()
        binary.write_bytes(b"different executable")
        with self.assertRaisesRegex(b.BuildError, "checksum mismatch"):
            b.cached_binary(self.prefix, self.request)

    def test_changed_source_or_compiler_cannot_reuse_existing_entry(self):
        self.install_cache()
        for field in ("revision", "compiler"):
            changed = dict(self.request, **{field: "different"})
            with self.subTest(field=field), self.assertRaisesRegex(b.BuildError, "provenance"):
                b.cached_binary(self.prefix, changed)
            self.assertNotEqual(b.cache_key(changed), b.cache_key(self.request))

    def test_incomplete_or_malformed_manifest_is_a_failure(self):
        self.prefix.mkdir()
        manifest = self.prefix / "build.json"
        for content in (None, "{broken", "{}"):
            if content is not None:
                manifest.write_text(content)
            with self.subTest(content=content), self.assertRaisesRegex(b.BuildError, "incomplete"):
                b.cached_binary(self.prefix, self.request)

    def test_missing_binary_does_not_silently_pass_provenance(self):
        binary = self.install_cache()
        binary.unlink()
        with self.assertRaisesRegex(b.BuildError, "incomplete"):
            b.cached_binary(self.prefix, self.request)

    def test_key_is_deterministic_and_covers_recipe_and_abi(self):
        self.assertEqual(b.cache_key(self.request), b.cache_key(dict(reversed(list(self.request.items())))))
        for field in ("recipe_sha256", "machine", "os_release"):
            self.assertNotEqual(b.cache_key(self.request), b.cache_key(dict(self.request, **{field: "changed"})))

    def test_capture_handles_programs_that_print_help_to_stderr(self):
        command = [sys.executable, "-c", "import sys; print('--debug-cmd', file=sys.stderr)"]
        self.assertEqual(b.capture(command, include_stderr=True), "--debug-cmd")
        self.assertEqual(b.capture(command), "")


if __name__ == "__main__":
    unittest.main()
