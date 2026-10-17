"""bin/abxcfg.py: host identity, hosts and repos resolution, path aliases, the agent environment.
Run: python3 -m unittest discover -s bin/tests"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

BIN = Path(__file__).resolve().parents[1]
if "abxcfg" not in sys.modules:
    os.environ["HOME"] = tempfile.mkdtemp(prefix="abx-cfg-test-")
sys.path.insert(0, str(BIN))
import abxcfg  # noqa: E402

ENV_KEYS = ("AGENT_BOX_OPTIONS", "AGENT_BOX_HOST", "CODER_AGENT_URL", "AGENT_BOX_SSH", "AGENT_BOX_REPO_URL")


class Config(unittest.TestCase):
    def setUp(self):
        self.saved = {k: os.environ.pop(k, None) for k in ENV_KEYS}
        abxcfg.agent_home().mkdir(parents=True, exist_ok=True)
        (abxcfg.agent_home() / "host").unlink(missing_ok=True)

    def tearDown(self):
        for k, v in self.saved.items():
            os.environ.pop(k, None)
            if v is not None:
                os.environ[k] = v
        (abxcfg.agent_home() / "host").unlink(missing_ok=True)

    def opts(self, **o):
        os.environ["AGENT_BOX_OPTIONS"] = json.dumps(o)


class HostIdentity(Config):
    def test_coder_env_is_the_box_and_anything_else_the_mac(self):
        os.environ["CODER_AGENT_URL"] = "https://coder"
        self.assertEqual(abxcfg.host_id(), "box")
        del os.environ["CODER_AGENT_URL"]
        self.assertEqual(abxcfg.host_id(), "mac")

    def test_env_then_host_file_then_detect_rules(self):
        self.opts(hosts={"lab": {"detect": {"env": "ABX_TEST_LAB"}}})
        os.environ["ABX_TEST_LAB"] = "1"
        try:
            self.assertEqual(abxcfg.host_id(), "lab")
            (abxcfg.agent_home() / "host").write_text("desk\n")
            self.assertEqual(abxcfg.host_id(), "desk")
            os.environ["AGENT_BOX_HOST"] = "box"
            self.assertEqual(abxcfg.host_id(), "box")
        finally:
            del os.environ["ABX_TEST_LAB"]

    def test_the_defaults_are_the_original_two_machines(self):
        box, mac = abxcfg.host("box"), abxcfg.host("mac")
        self.assertEqual((box["ssh"], box["sc_store"], box["sessions_branch"]), ("coder.tj-agents", "sc.json", "sessions/box"))
        self.assertEqual((mac["ssh"], mac["sc_store"], mac["sessions_branch"]), (None, "sc-mac.json", "sessions/mac"))
        self.assertEqual((box["sync_transcripts"], mac["sync_transcripts"]), ("all", "registry"))
        self.assertTrue(box["supervise"] and not mac["supervise"])

    def test_box_ssh_takes_the_apps_ssh_host_then_the_env(self):
        os.environ["AGENT_BOX_SSH"] = "from-env"
        self.assertEqual(abxcfg.host("box")["ssh"], "from-env")
        self.opts(ssh_host="from-app")
        self.assertEqual(abxcfg.host("box")["ssh"], "from-app")
        self.opts(ssh_host="from-app", hosts={"box": {"ssh": "explicit"}})
        self.assertEqual(abxcfg.host("box")["ssh"], "explicit")

    def test_a_new_host_gets_its_own_branch_and_store(self):
        self.opts(hosts={"lab": {"ssh": "lab.local"}}, sessions_branch="backups/{host}")
        lab = abxcfg.host("lab")
        self.assertEqual((lab["ssh"], lab["sc_store"], lab["sessions_branch"], lab["label"]),
                         ("lab.local", "sc-lab.json", "backups/lab", "lab"))
        os.environ["AGENT_BOX_HOST"] = "mac"
        self.assertEqual(abxcfg.remote_host(), "box")

    def test_a_null_host_entry_removes_it(self):
        self.opts(hosts={"mac": None})
        self.assertNotIn("mac", abxcfg.hosts())


class Repos(Config):
    def test_defaults_and_overrides(self):
        self.assertEqual(list(abxcfg.repos()), ["redo", "internal-tools", "redo-data"])
        self.opts(repos={"redo-data": None, "app": {"primary": "~/src/app"}}, default_repo="app")
        self.assertEqual(list(abxcfg.repos()), ["redo", "internal-tools", "app"])
        self.assertEqual(abxcfg.default_repo(), "app")
        self.assertEqual(abxcfg.primary("app"), Path.home() / "src" / "app")

    def test_folders_map_to_repo_keys_and_unknown_ones_keep_their_name(self):
        self.assertEqual(abxcfg.repo_for_folder("redotech-internal-tools"), "internal-tools")
        self.assertEqual(abxcfg.repo_for_folder("redo"), "redo")
        self.assertEqual(abxcfg.repo_for_folder("side-project"), "side-project")
        self.assertEqual(abxcfg.hue("redo"), 210.0)
        self.assertEqual(abxcfg.hue("side-project"), abxcfg.hue("side-project"))
        self.assertNotEqual(abxcfg.hue("side-project"), abxcfg.hue("other-project"))

    def test_repo_of_a_primary_checkout(self):
        folder = Path.home() / "src" / "thing"
        folder.mkdir(parents=True, exist_ok=True)
        self.opts(repos={"thing": {"primary": "~/src/thing"}})
        self.assertEqual(abxcfg.repo_of(folder), "thing")
        self.assertIsNone(abxcfg.repo_of(Path.home()))

    def test_worktree_paths_follow_the_template(self):
        self.assertEqual(abxcfg.worktree_path("redo", "tj/x-1"), Path.home() / "dev" / "redo-worktrees" / "tj-x-1")
        self.assertEqual(abxcfg.worktree_path("redo-data", "tj/x", "sc-abc123"),
                         Path.home() / "dev" / "redo-data-worktrees" / "sc-abc123")
        self.assertEqual(abxcfg.worktree_path("redo-data", "tj/x"), Path.home() / "dev" / "redo-data-worktrees" / "tj-x")
        self.assertEqual(abxcfg.worktrees_base("redo"), Path.home() / "dev" / "redo-worktrees")
        self.opts(repos={"app": {"primary": "~/src/app"}})
        self.assertEqual(abxcfg.worktree_path("app", "feat/y"), Path.home() / "src" / "app-worktrees" / "feat-y")

    def test_worktree_tool_templates(self):
        tool = abxcfg.worktree_tool("redo")
        tool["bin"] = "wt"
        self.assertEqual(abxcfg.tool_argv("redo", tool, "create", branch="b", path="/p"), ["wt", "b", "--no-start"])
        self.assertEqual(abxcfg.tool_argv("redo", tool, "remove", path="/p", force=""), ["wt", "remove", "/p"])
        self.assertEqual(abxcfg.tool_argv("redo", tool, "remove", path="/p", force="--force"), ["wt", "remove", "/p", "--force"])
        env = abxcfg.tool_env("redo", tool)
        self.assertEqual(env["WT_BASE"], str(Path.home() / "dev" / "redo-worktrees"))
        self.assertIsNone(abxcfg.worktree_tool("redo-data"))

    def test_default_base_reads_origin_head(self):
        repo = Path(tempfile.mkdtemp(dir=Path.home()))
        run = lambda *a: subprocess.run(["git", "-C", str(repo), *a], capture_output=True, check=True)  # noqa: E731
        run("init", "-q", "-b", "trunk")
        run("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "x")
        self.assertEqual(abxcfg.default_base(repo), "main")
        run("update-ref", "refs/remotes/origin/trunk", "HEAD")
        run("symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/trunk")
        self.assertEqual(abxcfg.default_base(repo), "trunk")
        self.opts(default_base="develop")
        self.assertEqual(abxcfg.default_base(None), "develop")
        shutil.rmtree(repo)


class Paths(Config):
    def test_aliases_map_the_same_checkout_and_then_the_home(self):
        pairs = abxcfg.alias_pairs("mac", "box", "/Users/me", "/home/coder")
        self.assertEqual(pairs, [("/Users/me/dev/redo", "/home/coder/project"), ("/Users/me", "/home/coder")])
        t = lambda p, a="mac", b="box", ha="/Users/me", hb="/home/coder": abxcfg.translate(p, a, b, ha, hb)  # noqa: E731
        self.assertEqual(t("/Users/me/dev/redo/apps/x"), "/home/coder/project/apps/x")
        self.assertEqual(t("/Users/me/dev/redo-worktrees/b"), "/home/coder/dev/redo-worktrees/b")
        self.assertEqual(t("/home/coder/project", "box", "mac", "/home/coder", "/Users/me"), "/Users/me/dev/redo")
        self.assertEqual(t("/opt/x"), "/opt/x")

    def test_pair_lists_and_unknown_hosts(self):
        self.opts(path_aliases=[["~/code", "~/work"]])
        self.assertEqual(abxcfg.alias_pairs("mac", "box", "/m", "/b")[0], ("/m/code", "/b/work"))
        self.assertEqual(abxcfg.alias_pairs("mac", "lab", "/m", "/l"), [("/m", "/l")])


class Environment(Config):
    def test_box_agent_env_adds_the_shim_and_wt(self):
        e = abxcfg.agent_env(abxcfg.host("box"))
        self.assertEqual(e["set"]["WT_PRIMARY"], "~/dev/redo")
        self.assertEqual(e["set"]["REDO_GITLAB_USER_AUTH"], "1")
        self.assertIn("SUPER_ENGINEERING_", e["unset_prefixes"])
        self.assertIn("CLAUDECODE", e["unset"])
        self.assertEqual(abxcfg.agent_env(abxcfg.host("mac"))["set"], {})

    def test_agent_env_sh_runs(self):
        os.environ["AGENT_BOX_HOST"] = "box"
        script = abxcfg.agent_env_sh() + '\necho "$WT_BASE|${CLAUDECODE:-gone}|${SUPER_ENGINEERING_X:-gone}"'
        env = dict(os.environ, CLAUDECODE="1", SUPER_ENGINEERING_X="1", PATH="/usr/bin:/bin")
        out = subprocess.run(["bash", "-c", script], capture_output=True, text=True, env=env).stdout.strip()
        self.assertEqual(out, f"{Path.home()}/dev/redo-worktrees|gone|gone")

    def test_no_foreign_tools(self):
        self.opts(foreign_tools=None)
        self.assertIsNone(abxcfg.foreign_tools())
        self.assertNotIn("SUPER_ENGINEERING_", abxcfg.agent_env(abxcfg.host("box"))["unset_prefixes"])

    def test_shell_exports(self):
        os.environ["AGENT_BOX_HOST"] = "box"
        self.opts(git_author={"name": "bot ({host})", "email": "bot@example.com"}, state_branch="st",
                  state_repo_url="https://example.com/s.git")
        out = subprocess.run(["bash", "-c", abxcfg.shell() + '\necho "$AGENT_HOST $SESSIONS_BRANCH $STATE_BRANCH '
                              '$GIT_AUTHOR_NAME $GIT_AUTHOR_EMAIL $AGENT_BOX_REPO_URL $AGENT_SYNC_TRANSCRIPTS"'],
                             capture_output=True, text=True).stdout.strip()
        self.assertEqual(out, "box sessions/box st bot (box) bot@example.com https://example.com/s.git all")


if __name__ == "__main__":
    unittest.main()
