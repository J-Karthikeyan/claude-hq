"""ops/release.sh waits for CI that is still running instead of failing.

A deploy that starts right after a merge reaches the CI gate while the checks
are still in progress. The gate used to read that as "CI has not passed" and
fail, and autodeploy then would not retry the commit for five minutes -- so a
green commit sat undeployed, behind an error that said CI had failed.

Each case runs the real script in a scratch repo with curl, sleep and docker
faked on PATH; docker fails, so a release that gets past the gate stops at the
build instead of touching anything.
"""
import json
import os
import shutil
import subprocess
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def checks(**runs):
    """{name: "in_progress" | conclusion} -> a check-runs API body."""
    return json.dumps({"check_runs": [
        {"name": n, "status": "in_progress", "conclusion": None} if s == "in_progress"
        else {"name": n, "status": "completed", "conclusion": s}
        for n, s in runs.items()]})


class CiGate(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.tmp)
        repo = os.path.join(self.tmp, "repo")
        os.makedirs(os.path.join(repo, "ops"))
        os.makedirs(os.path.join(repo, "backend"))
        shutil.copy(os.path.join(ROOT, "ops", "release.sh"), os.path.join(repo, "ops"))
        git = ["git", "-C", repo, "-c", "user.name=t", "-c", "user.email=t@t"]
        subprocess.run(git[:3] + ["init", "-q"], check=True)
        subprocess.run(git + ["add", "."], check=True)
        subprocess.run(git + ["commit", "-qm", "x"], check=True)
        self.repo = repo

        self.bin = os.path.join(self.tmp, "bin")
        os.makedirs(self.bin)
        self.responses = os.path.join(self.tmp, "responses")
        # curl answers with the next queued response, repeating the last one;
        # an empty response is a failed request.
        self._tool("curl", f"""
r={self.responses}
line=$(head -1 "$r"); [ "$(wc -l < "$r")" -gt 1 ] && sed -i.bak 1d "$r"
[ -n "$line" ] || exit 22
echo "$line"
""")
        self._tool("sleep", "exit 0")
        self._tool("docker", "exit 1")

    def _tool(self, name, body):
        path = os.path.join(self.bin, name)
        with open(path, "w") as f:
            f.write("#!/usr/bin/env bash\n" + body)
        os.chmod(path, 0o755)

    def deploy(self, *responses, wait="300"):
        with open(self.responses, "w") as f:
            f.write("\n".join(responses) + "\n")
        env = dict(os.environ, PATH=self.bin + os.pathsep + os.environ["PATH"],
                   ARENA_SKIP_PULL="1", ARENA_CI_WAIT=wait)
        p = subprocess.run(["bash", os.path.join(self.repo, "ops", "release.sh"), "deploy"],
                           env=env, capture_output=True, text=True, timeout=60)
        return p.stdout + p.stderr

    def test_waits_for_running_ci_then_releases(self):
        out = self.deploy(checks(unittest="success", backend="in_progress", rust="in_progress"),
                          checks(unittest="success", backend="success", rust="in_progress"),
                          checks(unittest="success", backend="success", rust="success"))
        self.assertIn("holding for CI, up to 300s", out)
        self.assertIn("waiting on: backend, rust (passed: unittest)", out)
        self.assertEqual(out.count("holding for CI"), 1)
        self.assertIn("CI passed", out)
        self.assertIn("Building rs", out)

    def test_waits_for_ci_that_has_not_started(self):
        out = self.deploy(json.dumps({"check_runs": []}), checks(unittest="success"))
        self.assertIn("CI passed", out)

    def test_gives_up_when_ci_runs_past_the_wait(self):
        out = self.deploy(checks(rust="in_progress"), wait="0")
        self.assertIn("CI still running after 0s", out)
        self.assertNotIn("Building", out)

    def test_a_failed_check_fails_without_waiting(self):
        out = self.deploy(checks(unittest="failure", rust="in_progress"))
        self.assertIn("CI failed for this commit", out)
        self.assertNotIn("waiting on", out)
        self.assertNotIn("Building", out)

    def test_github_unreachable_says_so(self):
        out = self.deploy("")
        self.assertIn("could not reach GitHub", out)
        self.assertNotIn("Building", out)


if __name__ == "__main__":
    unittest.main()
