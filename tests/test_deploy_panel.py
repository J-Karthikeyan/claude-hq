"""Token tests for ops/deploy_panel.py.

The panel signs three kinds of token with one HMAC helper: a session cookie,
a CSRF token and an OAuth state. Two properties matter.

Round-trip: a token the panel issues must verify. A CSRF check that can never
pass locks every POST — deploy, rebuild, restart — behind a 403.

Domain separation: the "csrf:" prefix lives *inside* the signed payload so a
stolen session cookie cannot be replayed as a CSRF token, or the reverse. A
verifier that strips the prefix before checking the MAC destroys both
properties at once.

Stdlib only. Run with:
    python3 -m unittest discover -s tests
"""
import importlib.util
import os
import sys
import time
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

# deploy_panel reads its config at import time, so the environment has to be
# set first. It also lives under ops/ rather than being importable by name.
os.environ.setdefault("PANEL_SECRET_KEY", "test-key-not-a-real-secret")
os.environ.setdefault("PANEL_ALLOWED_USERS", "alice,bob")

_spec = importlib.util.spec_from_file_location(
    "deploy_panel", os.path.join(ROOT, "ops", "deploy_panel.py"))
deploy_panel = importlib.util.module_from_spec(_spec)
sys.modules["deploy_panel"] = deploy_panel
_spec.loader.exec_module(deploy_panel)

_sign = deploy_panel._sign
_verify = deploy_panel._verify
TTL = deploy_panel.SESSION_TTL


def session_token(login, ttl=TTL):
    """Exactly what /auth/callback issues."""
    return _sign(f"{login}|{time.time() + ttl}")


def csrf_token(login, ttl=TTL):
    """Exactly what Handler._csrf issues."""
    return _sign(f"csrf:{login}|{time.time() + ttl}")


class RoundTrip(unittest.TestCase):
    def test_session_cookie_verifies(self):
        self.assertEqual(_verify(session_token("alice")), "alice")

    def test_csrf_token_verifies(self):
        # The regression this file exists for: do_POST used to strip the
        # "csrf:" prefix before verifying, so the MAC was recomputed over a
        # different string and no CSRF token could ever be accepted.
        self.assertEqual(_verify(csrf_token("alice"), "csrf"), "alice")

    def test_verifies_for_every_allowed_user(self):
        for login in ("alice", "bob"):
            self.assertEqual(_verify(csrf_token(login), "csrf"), login)


class DomainSeparation(unittest.TestCase):
    def test_csrf_token_is_not_a_session_cookie(self):
        self.assertIsNone(_verify(csrf_token("alice")))

    def test_session_cookie_is_not_a_csrf_token(self):
        self.assertIsNone(_verify(session_token("alice"), "csrf"))

    def test_wrong_purpose_is_rejected(self):
        self.assertIsNone(_verify(csrf_token("alice"), "state"))


class Rejections(unittest.TestCase):
    def test_expired_token(self):
        self.assertIsNone(_verify(csrf_token("alice", ttl=-1), "csrf"))
        self.assertIsNone(_verify(session_token("alice", ttl=-1)))

    def test_tampered_mac(self):
        tok = csrf_token("alice")
        flipped = tok[:-1] + ("0" if tok[-1] != "0" else "1")
        self.assertIsNone(_verify(flipped, "csrf"))

    def test_tampered_login(self):
        # Re-signing is required to change the login; editing it is not enough.
        tok = csrf_token("alice").replace("alice", "bob", 1)
        self.assertIsNone(_verify(tok, "csrf"))

    def test_user_not_in_allowlist(self):
        # Checked on every request, so removing a login revokes it at once.
        self.assertIsNone(_verify(csrf_token("mallory"), "csrf"))

    def test_malformed_tokens(self):
        for junk in ("", "no-dot", "a.b", "|.", "csrf:alice", "....", "x|y.z"):
            with self.subTest(token=junk):
                self.assertIsNone(_verify(junk))
                self.assertIsNone(_verify(junk, "csrf"))


if __name__ == "__main__":
    unittest.main()


# A colourised line exactly as ops/release.sh's say() prints it.
def say(text):
    return "\x1b[1;34m==>\x1b[0m " + text


RUN = "\n".join([
    "2026-10-09T06:50:01Z  new commits 4133b25 -> 843e4c6",
    "2026-10-09T06:50:01Z  releasing with ops/release.sh (impl from backend/.release.env)",
    say("Fetching main"),
    "    \x1b[32m✓\x1b[0m at 843e4c6: Merge pull request #96",
    say("Checking GitHub CI for 843e4c6"),
    say("Building rs 2026.10.09-843e4c6"),
]) + "\n"


class DeployProgress(unittest.TestCase):
    """The progress bar reads release.sh's own stage announcements.

    A bar that runs on a timer is worse than no bar on a console someone
    watches during an incident: it keeps climbing while the deploy is wedged,
    and it reaches 100% on a release that failed. Every number here comes from
    a stage that has actually been announced.
    """

    def p(self, text):
        return deploy_panel.parse_progress(text)

    def test_it_counts_the_stages_that_have_happened(self):
        r = self.p(RUN)
        self.assertEqual((r["phase"], r["idx"], r["pct"], r["state"]),
                         ("BUILD", 3, 50, "running"))

    def test_colour_escapes_do_not_hide_a_stage(self):
        """say() colourises, so an unstripped parser sees no stages at all."""
        self.assertIn("\x1b[", RUN)
        self.assertEqual(self.p(RUN)["idx"], 3)

    def test_a_finished_release_reads_as_done(self):
        r = self.p(RUN + say("Starting rs v") + "\n" + say("Released 2026.10.09-843e4c6 (rs)") + "\n")
        self.assertEqual((r["state"], r["pct"]), ("ok", 100))

    def test_a_failed_release_freezes_where_it_got_to(self):
        """The property that matters most: a failure must NOT read as finished."""
        r = self.p(RUN + "    \x1b[31m✗\x1b[0m build failed\n"
                   + say("Rolling back to 2026.10.08-4133b25 (rs)") + "\n")
        self.assertEqual(r["state"], "fail")
        self.assertEqual(r["pct"], 50)
        self.assertLess(r["pct"], 100)

    def test_an_earlier_run_in_the_log_does_not_count(self):
        """The log is appended to forever; only the newest run is the subject."""
        old = RUN + say("Released 2026.10.08-old (rs)") + "\n"
        r = self.p(old + RUN)
        self.assertEqual((r["idx"], r["state"]), (3, "running"))

    def test_nothing_yet_is_indeterminate_rather_than_zero_percent(self):
        for text in ("", "2026-10-09T06:50:01Z  new commits a -> b\n"):
            r = self.p(text)
            self.assertEqual(r["idx"], 0)
            self.assertEqual(r["pct"], 0)
            self.assertIsNone(r["phase"])

    def test_a_skipped_stage_does_not_stall_the_bar(self):
        """A Python release runs no migration, so MIGRATE never appears."""
        r = self.p(RUN + say("Starting py 2026.10.09-843e4c6") + "\n")
        self.assertEqual((r["phase"], r["pct"]), ("START", 83))

    def test_an_unrelated_stage_line_does_not_un_advance(self):
        """release.sh prints ==> lines that are not stages (a rollback banner,
        the status report). None may drag the bar back."""
        r = self.p(RUN + say("Rolling back to 2026.10.08-4133b25 (rs)") + "\n")
        self.assertEqual(r["idx"], 3)

    def test_a_second_fetch_is_a_new_run_and_resets(self):
        """Not a contradiction of the above: ==> Fetching only ever starts a
        release, so seeing it again means a SECOND deploy began. The bar has to
        follow the new one rather than keep showing the old one's progress."""
        r = self.p(RUN + say("Fetching main") + "\n")
        self.assertEqual((r["phase"], r["idx"], r["state"]), ("FETCH", 1, "running"))


GATE = RUN.split(say("Building"))[0]   # stopped at the CI gate


class CiHold(unittest.TestCase):
    """release.sh holds at the CI gate while GitHub is still running checks.

    The panel turns that into its own state rather than a stalled bar, and it
    must never call a release held at the gate "aborted" or "rolled back":
    nothing on the box was touched.
    """

    def p(self, *lines):
        return deploy_panel.parse_progress(GATE + "\n".join(lines) + "\n")

    def test_a_hold_reads_its_checks_budget_and_time(self):
        r = self.p("    holding for CI, up to 300s",
                   "    waiting on: backend, rust (passed: unittest)",
                   "    waiting on: rust (passed: unittest, backend)")
        self.assertEqual((r["phase"], r["state"]), ("CI GATE", "running"))
        ci = r["ci"]
        self.assertEqual((ci["state"], ci["sha"], ci["budget"], ci["waited"]),
                         ("waiting", "843e4c6", 300, 20))
        self.assertEqual(ci["checks"], [{"name": "unittest", "status": "success"},
                                        {"name": "backend", "status": "success"},
                                        {"name": "rust", "status": "pending"}])

    def test_checks_not_queued_yet_still_hold(self):
        r = self.p("    holding for CI, up to 300s", "    no CI runs for this commit yet, waiting")
        self.assertEqual((r["ci"]["state"], r["ci"]["checks"]), ("waiting", []))

    def test_cleared_gate_moves_on_and_keeps_the_results(self):
        r = deploy_panel.parse_progress(GATE + "    waiting on: rust (passed: unittest)\n"
                                        "    unittest   success\n    rust       success\n"
                                        "    \x1b[32m✓\x1b[0m CI passed\n" + say("Building rs x") + "\n")
        self.assertEqual((r["phase"], r["ci"]["state"]), ("BUILD", "passed"))
        self.assertEqual({c["status"] for c in r["ci"]["checks"]}, {"success"})

    def test_why_it_stopped_is_named(self):
        for line, reason in (("CI failed for this commit (x)", "ci_failed"),
                             ("CI still running after 300s (x)", "ci_timeout"),
                             ("could not reach GitHub to check CI (x)", "ci_unreachable")):
            r = self.p("    unittest   failure", "    \x1b[31m✗\x1b[0m " + line)
            self.assertEqual((r["state"], r["reason"]), ("fail", reason), line)
            self.assertIn("Nothing was deployed", deploy_panel.FAILURES[reason])

    def test_a_failing_check_keeps_its_conclusion(self):
        r = self.p("    unittest   success", "    backend    failure", "    rust       in_progress",
                   "    \x1b[31m✗\x1b[0m CI failed for this commit (x)")
        self.assertEqual([c["status"] for c in r["ci"]["checks"]], ["success", "failure", "pending"])

    def test_only_a_rollback_is_aborted(self):
        r = deploy_panel.parse_progress(RUN + "    \x1b[31m✗\x1b[0m build failed\n"
                                        + say("Rolling back to 2026.10.08-4133b25 (rs)") + "\n")
        self.assertEqual(r["reason"], "rolled_back")
        page = deploy_panel.PAGE
        self.assertEqual(page.count(':"aborted'), 1)   # one verdict says it
        self.assertIn('rolled_back:"aborted', page)

    def test_a_running_release_has_no_reason(self):
        self.assertIsNone(deploy_panel.parse_progress(RUN)["reason"])

    def test_the_hold_has_its_own_look_and_respects_reduced_motion(self):
        page = deploy_panel.PAGE
        for name in ("spin", "ping", "comet", "rgbsplit", "lockon", "clearedin"):
            self.assertIn("@keyframes " + name, page)
        block = _media_blocks(page, "@media (prefers-reduced-motion:reduce)")
        for sel in (".beam", ".lane .lt::before", ".ticker span", ".holdtitle"):
            self.assertIn(sel, block)


def _media_blocks(css, header):
    """Every @media rule with this header, joined.

    There is more than one -- each sits next to the rules it modifies -- and a
    test that reads only the first would check the wrong half. Braces are
    matched rather than assuming how the block is indented.
    """
    out, at = [], 0
    while True:
        k = css.find(header, at)
        if k < 0:
            break
        i = css.index("{", k + len(header))
        depth, j = 0, i
        while j < len(css):
            if css[j] == "{":
                depth += 1
            elif css[j] == "}":
                depth -= 1
                if depth == 0:
                    out.append(css[i + 1:j])
                    break
            j += 1
        else:
            raise AssertionError("unterminated @media block")
        at = j
    assert out, "no " + header + " block at all"
    return "\n".join(out)


class DeployEnding(unittest.TestCase):
    """A finished deploy gets a sequence, and the two are nothing alike.

    These are structural checks on the page, not a rendering test -- they exist
    for the one regression that would be silent and costly: reduced motion must
    switch the celebration OFF while leaving the RESULT on screen. A blanket
    `display:none` in that media query would hide whether the deploy worked
    from exactly the people most likely to have the setting on.
    """

    PAGE = deploy_panel.PAGE

    def test_success_and_failure_have_separate_sequences(self):
        for cls in ("done-ok", "done-fail"):
            self.assertIn(cls, self.PAGE)
        # Nothing alike: a sweep for one, a vignette alarm and a shake for the
        # other, and they must not share a keyframe name.
        for name in ("sweep", "chipflash", "stampin", "alarmv", "stampbad", "hardshake"):
            self.assertIn("@keyframes " + name, self.PAGE)

    def test_reduced_motion_keeps_the_verdict_visible(self):
        block = _media_blocks(self.PAGE, "@media (prefers-reduced-motion:reduce)")
        self.assertIn("animation:none", block)
        self.assertNotIn(".verdict{display:none", block.replace(" ", ""))

    def test_the_ending_fires_once(self):
        """paint() runs on every poll and again on the final settle; a sweep
        that restarts each time reads as a second deploy starting."""
        self.assertIn("if(ENDED) return; ENDED=true;", self.PAGE)

    def test_the_post_decides_the_outcome_not_the_poll(self):
        """A last poll can read "running" off a log the release has not
        finished flushing, so the ending is fired from the POST's own answer."""
        self.assertIn("finish(j.ok", self.PAGE)


class ReleasedShaTests(unittest.TestCase):
    """The panel's "running" is the released commit, not the checkout's HEAD."""

    def test_reads_the_sha_off_the_release_version(self):
        env = "# Written by ops/release.sh\nARENA_IMPL=rs\nARENA_VERSION=2026.10.08-dd04883\nARENA_APP_IMAGE=x\n"
        self.assertEqual(deploy_panel.released_sha(env), "dd04883")

    def test_no_release_yet_is_empty(self):
        self.assertEqual(deploy_panel.released_sha(""), "")
        self.assertEqual(deploy_panel.released_sha("ARENA_VERSION=local\n"), "")
        self.assertEqual(deploy_panel.released_sha(None), "")


class BuildStageProgress(unittest.TestCase):
    """The build stage is the long one, and it used to be the blind one.

    `/api/progress` read the last 160 lines of the deploy log. A release build
    compiles hundreds of crates and BuildKit prints a line for each, so
    `==> Building` scrolled out of that window within seconds -- the parser
    then saw no stage at all and the bar fell back to indeterminate. It went
    blank exactly when there was most to wait for.
    """

    def build_log(self, crates=0, step=(8, 8), secs=None):
        out = [say("Fetching main"), say("Checking GitHub CI for abc1234"),
               say("Building rs 2026.10.09-abc1234"),
               "#9 [app build 4/8] COPY backend-rs/src backend-rs/src",
               "#%d [app build %d/%d] RUN cd backend-rs && cargo build --release"
               % (13, step[0], step[1])]
        for i in range(crates):
            out.append("#13 %6.2f   Compiling crate%d v1.0.0" % (1.5 * i, i))
        if secs is not None:
            out.append("#13 %6.2f   Compiling tail v1.0.0" % secs)
        return "\n".join(out) + "\n"

    def test_the_stage_survives_a_long_build(self):
        """The regression: 400 crates of output must not hide the stage."""
        r = deploy_panel.parse_progress(self.build_log(crates=400))
        self.assertEqual(r["phase"], "BUILD")
        self.assertEqual(r["pct"], 50)

    def test_the_old_window_is_what_broke_it(self):
        """Keeps the cause on record: the same log, cut to 160 lines, is blind."""
        clipped = "\n".join(self.build_log(crates=400).splitlines()[-160:])
        r = deploy_panel.parse_progress(clipped)
        self.assertIsNone(r["phase"])
        self.assertEqual(r["pct"], 0)

    def test_it_reports_the_build_step(self):
        r = deploy_panel.parse_progress(self.build_log(step=(6, 8)))
        self.assertEqual(r["sub"]["pct"], 62)          # 5 of 8 done
        self.assertIn("step 6 of 8", r["sub"]["note"])

    def test_crates_refine_the_step_when_a_previous_build_sized_it(self):
        prev = self.build_log(crates=400).replace("Fetching main", "Fetching main") \
            + say("Starting rs v") + "\n"
        now = prev + self.build_log(crates=100)
        r = deploy_panel.parse_progress(now)
        self.assertIn("100 of ~400 crates", r["sub"]["note"])
        # 7 of 8 steps done, plus a quarter of the last one.
        self.assertEqual(r["sub"]["pct"], round(100 * (7 + 0.25) / 8))

    def test_without_a_previous_build_it_counts_but_does_not_guess(self):
        r = deploy_panel.parse_progress(self.build_log(crates=100))
        self.assertIn("100 crates compiled", r["sub"]["note"])
        self.assertNotIn("~", r["sub"]["note"])

    def test_it_reads_seconds_into_the_step_from_buildkit(self):
        r = deploy_panel.parse_progress(self.build_log(crates=5, secs=127.8))
        self.assertAlmostEqual(r["sub"]["secs"], 127.8)

    def test_other_stages_have_no_sub_progress(self):
        r = deploy_panel.parse_progress(
            self.build_log(crates=20) + say("Migrating the database") + "\n")
        self.assertEqual(r["phase"], "MIGRATE")
        self.assertIsNone(r["sub"])

    def test_a_finished_build_reports_no_sub_progress(self):
        done = self.build_log(crates=50) + say("Released 2026.10.09-abc1234 (rs)") + "\n"
        r = deploy_panel.parse_progress(done)
        self.assertEqual(r["state"], "ok")
        self.assertIsNone(r["sub"])

    def test_the_tail_reader_starts_on_a_line_boundary(self):
        import tempfile, os as _os
        fd, path = tempfile.mkstemp()
        try:
            with _os.fdopen(fd, "w") as f:
                f.write("".join("line %04d padding padding padding\n" % i for i in range(5000)))
            tail = deploy_panel.read_log_tail(path, limit=2000)
            self.assertTrue(len(tail) <= 2000)
            # No half-eaten first line.
            self.assertTrue(tail.startswith("line "), tail[:40])
        finally:
            _os.unlink(path)
