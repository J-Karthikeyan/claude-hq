"""Everything the Rust Arena embeds at compile time must reach its Docker build.

`include_str!` is resolved by rustc, not by COPY, so a file that never arrives
fails the image build with "couldn't read ...: No such file or directory" from
the compiler -- a long way from the Dockerfile that caused it. The Rust image
also builds from the REPO ROOT with an ignore file that excludes everything and
then allowlists, so each embedded path needs a line in two places.

This test is here because golf and the creature duel were added with their data
files embedded the same way the other three games do it, and the image build was
the only thing that noticed.
"""
import os
import re
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RS = os.path.join(ROOT, "backend")


def read(*p):
    with open(os.path.join(*p), encoding="utf-8") as f:
        return f.read()


def embedded_paths():
    """Every path an include_str!/include_bytes! in backend/src reaches for,
    relative to backend/ (the Docker build context)."""
    out = set()
    for dirpath, _dirs, files in os.walk(os.path.join(RS, "src")):
        for name in files:
            if not name.endswith(".rs"):
                continue
            src = read(dirpath, name)
            for rel in re.findall(r'include_(?:str|bytes)!\("([^"]+)"\)', src):
                abs_path = os.path.normpath(os.path.join(dirpath, rel))
                out.add(os.path.relpath(abs_path, RS))
    return out


class RustBuildContext(unittest.TestCase):
    def test_every_embedded_file_exists(self):
        missing = [p for p in embedded_paths() if not os.path.isfile(os.path.join(RS, p))]
        self.assertEqual(missing, [], "include_str! points at a file that is not there")

    def test_everything_embedded_is_inside_the_build_context(self):
        """The image builds from backend/ alone; a file outside it never arrives."""
        outside = sorted(p for p in embedded_paths() if p.startswith(".."))
        self.assertEqual(outside, [], "embedded files must live under backend/ (e.g. backend/data/)")

    def test_embedded_files_are_not_ignored_by_the_docker_build(self):
        """The ignore file is `*` plus an allowlist, so each path needs a `!` line."""
        allowed = {line[1:].strip() for line in read(RS, ".dockerignore").splitlines() if line.startswith("!")}
        for p in sorted(embedded_paths()):
            # A `!dir` line covers everything under it.
            covered = any(p == a or p.startswith(a.rstrip("/") + "/") for a in allowed)
            self.assertTrue(covered, f"{p} is embedded but excluded from the build context")

    def test_embedded_files_are_copied_into_the_image(self):
        df = read(RS, "Dockerfile").replace("\\\n", " ")
        sources = set()
        for line in df.splitlines():
            if line.startswith("COPY ") and "--from" not in line:
                sources.update(line.split()[1:-1])
        for p in sorted(embedded_paths()):
            covered = any(p == src or p.startswith(src.rstrip("/") + "/") for src in sources)
            self.assertTrue(covered, f"{p} is embedded but never COPYed into the image")


if __name__ == "__main__":
    unittest.main()
