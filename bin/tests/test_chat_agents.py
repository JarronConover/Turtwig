"""bin/chat's agent templates and Codex session discovery, against a temporary HOME.
Run: python3 -m unittest discover -s bin/tests"""

import importlib.machinery
import importlib.util
import json
import os
import shutil
import tempfile
import time
import unittest
from pathlib import Path

HOME = tempfile.mkdtemp(prefix="abx-chat-test-")
os.environ["HOME"] = HOME
os.environ.pop("CODER_AGENT_URL", None)
_loader = importlib.machinery.SourceFileLoader("chat", str(Path(__file__).resolve().parents[1] / "chat"))
_spec = importlib.util.spec_from_loader("chat", _loader)
chat = importlib.util.module_from_spec(_spec)
_loader.exec_module(chat)
chat.background = lambda *args: None


def tearDownModule():
    shutil.rmtree(HOME, ignore_errors=True)


def rollout(day, session, cwd, at, **meta):
    folder = Path(HOME, ".codex", "sessions", *day.split("-"))
    folder.mkdir(parents=True, exist_ok=True)
    path = folder / f"rollout-{day}T00-00-00-{session}.jsonl"
    stamp = time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime(at))
    payload = {"id": session, "cwd": cwd, "timestamp": stamp, "source": "cli", "thread_source": "user", **meta}
    path.write_text(json.dumps({"type": "session_meta", "payload": payload}) + "\n")
    os.utime(path, (at, at))
    return path


def register(entry):
    chat.REGISTRY.mkdir(parents=True, exist_ok=True)
    (chat.REGISTRY / f"{entry['id']}.json").write_text(json.dumps(entry))


class Templates(unittest.TestCase):
    def test_optional_groups_drop_when_empty(self):
        t = ["{bin}", "--session-id", "{id}", ["--model", "{model}"], ["{prompt}"]]
        self.assertEqual(chat.fill(t, {"bin": "claude", "id": "x", "model": None, "prompt": "hi"}),
                         ["claude", "--session-id", "x", "hi"])
        self.assertEqual(chat.fill(t, {"bin": "claude", "id": "x", "model": "opus", "prompt": ""}),
                         ["claude", "--session-id", "x", "--model", "opus"])

    def test_claude_argv_is_unchanged(self):
        v = {"bin": "claude", "id": "c1", "session": "c1", "from": "s0", "name": "n", "model": "opus", "prompt": "go"}
        self.assertEqual(chat.fill(chat.AGENTS["claude"]["new"], v),
                         ["claude", "--session-id", "c1", "--remote-control", "n", "--settings", chat.CLAUDE_SETTINGS, "--model", "opus", "go"])
        self.assertEqual(chat.fill(chat.AGENTS["claude"]["resume"], dict(v, model=None)),
                         ["claude", "--resume", "c1", "--remote-control", "n", "--settings", chat.CLAUDE_SETTINGS])
        self.assertEqual(chat.fill(chat.AGENTS["claude"]["fork"], v),
                         ["claude", "--resume", "s0", "--fork-session", "--session-id", "c1", "--remote-control", "n",
                          "--settings", chat.CLAUDE_SETTINGS, "--model", "opus"])

    def test_codex_trusts_the_folder_inline(self):
        argv = chat.fill(chat.AGENTS["codex"]["resume"], {"bin": "codex", "session": "s1", "realdir": "/w", "model": None})
        self.assertEqual(argv[:3], ["codex", "resume", "s1"])
        self.assertIn('projects={"/w"={trust_level="trusted"}}', argv)

    def test_a_chat_without_an_agent_is_claude(self):
        self.assertEqual(chat.session_of({"id": "c1"}), "c1")
        self.assertIsNone(chat.session_of({"id": "c2", "agent": "codex"}))


class Discovery(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.mkdtemp(dir=HOME)
        self.now = time.time()
        self.day = time.strftime("%Y-%m-%d", time.localtime(self.now))

    def test_finds_the_new_rollout_in_the_chats_folder(self):
        c = {"id": "c-new", "name": "n", "agent": "codex", "dir": self.dir, "launched_at": self.now - 10}
        register(c)
        rollout(self.day, "old", self.dir, self.now - 3600)
        rollout(self.day, "elsewhere", "/somewhere/else", self.now - 5)
        rollout(self.day, "sub", self.dir, self.now - 4, thread_source="subagent",
                source={"subagent": {"thread_spawn": {"parent_thread_id": "s-new"}}})
        mine = rollout(self.day, "s-new", self.dir, self.now - 8)
        self.assertEqual(chat.find_session(c), ("s-new", str(mine)))
        self.assertTrue(chat.record_session("c-new"))
        saved = chat.load("c-new")
        self.assertEqual((saved["session_id"], saved["transcript"]), ("s-new", str(mine)))
        self.assertEqual(chat.agent_transcript_paths(saved), [mine, Path(str(mine).replace("s-new", "sub"))])

    def test_a_moved_chat_finds_its_rollout_by_session(self):
        here = rollout(self.day, "s-moved", self.dir, self.now - 30)
        c = {"id": "c-moved", "agent": "codex", "session_id": "s-moved", "transcript": "/Users/elsewhere/rollout.jsonl"}
        self.assertEqual(chat.transcript(c), here)

    def test_skips_a_session_another_chat_has(self):
        rollout(self.day, "taken", self.dir, self.now - 2)
        register({"id": "c-other", "agent": "codex", "session_id": "taken"})
        c = {"id": "c-late", "name": "n", "agent": "codex", "dir": self.dir, "launched_at": self.now - 10}
        register(c)
        self.assertIsNone(chat.find_session(c))
        self.assertFalse(chat.record_session("c-late"))

    def test_a_fork_takes_the_rollout_forked_from_its_source(self):
        register({"id": "c-src", "agent": "codex", "session_id": "s-src", "dir": self.dir})
        rollout(self.day, "s-src", self.dir, self.now - 600)
        rollout(self.day, "s-plain", self.dir, self.now - 9)
        fork = rollout(self.day, "s-fork", self.dir, self.now - 8, forked_from_id="s-src")
        c = {"id": "c-fork", "agent": "codex", "dir": self.dir, "launched_at": self.now - 10, "forked_from": "c-src"}
        register(c)
        self.assertEqual(chat.find_session(c), ("s-fork", str(fork)))
        c.update(session_id="s-fork", transcript=str(fork))
        self.assertEqual([p.name for p in chat.agent_transcript_paths(c)],
                         [fork.name, fork.name.replace("s-fork", "s-src")])


class Generic(unittest.TestCase):
    def tearDown(self):
        os.environ.pop("AGENT_BOX_OPTIONS", None)

    def test_tmux_names_follow_the_options(self):
        self.assertEqual(chat.tmux_name({"id": "0123456789ab"}), "chat-01234567")
        os.environ["AGENT_BOX_OPTIONS"] = json.dumps({"tmux_prefix": "abx-", "tmux_id_chars": 4})
        self.assertEqual(chat.tmux_name({"id": "0123456789ab"}), "abx-0123")

    def test_trust_sets_the_agents_key_once(self):
        folder = tempfile.mkdtemp(dir=HOME)
        chat.trust_folder(chat.AGENTS["claude"], folder)
        cfg = json.loads(Path(HOME, ".claude.json").read_text())
        self.assertTrue(cfg["projects"][folder]["hasTrustDialogAccepted"])
        custom = {"trust": {"file": "~/.tool/trust.json", "path": ["trusted", "{dir}"], "value": "yes"}}
        chat.trust_folder(custom, folder)
        self.assertEqual(json.loads(Path(HOME, ".tool", "trust.json").read_text()), {"trusted": {folder: "yes"}})
        chat.trust_folder({"trust": None}, folder)

    def test_a_symlinked_folders_transcript_is_under_its_target(self):
        target = tempfile.mkdtemp(dir=HOME)
        link = Path(HOME, "linked-repo")
        link.symlink_to(target)
        filed = chat.PROJECTS / chat.project_slug(os.path.realpath(target)) / "c-link.jsonl"
        filed.parent.mkdir(parents=True, exist_ok=True)
        filed.write_text("{}\n")
        self.assertEqual(chat.transcript({"id": "c-link", "dir": str(link)}), filed)

    def test_backup_roots_cover_every_agent(self):
        roots = dict(chat.backup_roots())
        self.assertEqual(roots[f"{HOME}/.claude/projects"], "projects")
        self.assertEqual(roots[f"{HOME}/.codex/sessions"], "codex-sessions")
        os.environ["AGENT_BOX_OPTIONS"] = json.dumps({"agents": {"codex": {"backup": False}}})
        self.assertEqual(list(dict(chat.backup_roots()).values()), ["projects"])

    def test_the_kind_a_fork_gets(self):
        self.assertIn("orchestrator", chat.chat_kinds())
        os.environ["AGENT_BOX_OPTIONS"] = json.dumps({"chat_kinds": ["solo"]})
        self.assertEqual(chat.chat_kinds(), ["solo"])


class Dispatchers(unittest.TestCase):
    """dispatcher_parent: which /ship-feature parent a relaunched chat resumes, from its locks and ledger."""
    LEDGER = Path(HOME, ".local", "state", "redo", "ship-feature")

    def setUp(self):
        shutil.rmtree(self.LEDGER, ignore_errors=True)
        self.LEDGER.mkdir(parents=True)

    def lock(self, name, session, at):
        (self.LEDGER / f"{name}.lock").write_text(json.dumps({"pid": 1, "session": session, "at": at}))

    def ledger(self, parent, printed=None, paused=None):
        events = {k: {"digest": m[0], "at": m[1]} for k, m in (("printed", printed), ("paused", paused)) if m}
        (self.LEDGER / f"{parent}.json").write_text(json.dumps({"events": events}))

    def test_the_newest_lock_the_session_owns(self):
        self.lock("AIOPSUP-1", "s1", 100)
        self.lock("AIOPSUP-2", "s1", 200)
        self.lock("AIOPSUP-3", "s2", 300)
        self.lock("AIOPSUP-9.feature", "s1", 900)
        self.lock("AIOPSUP-9.watch", "s1", 900)
        for p in ("AIOPSUP-1", "AIOPSUP-2", "AIOPSUP-3"):
            self.ledger(p, ("AIOPSUP-5=verifying AIOPSUP-6=your review | ", 1))
        self.assertEqual(chat.dispatcher_parent("s1"), "AIOPSUP-2")
        self.assertIsNone(chat.dispatcher_parent("s3"))
        self.assertIsNone(chat.dispatcher_parent(None))

    def test_a_finished_or_unknown_feature_is_left_alone(self):
        self.lock("AIOPSUP-1", "s1", 100)
        self.assertIsNone(chat.dispatcher_parent("s1"))
        self.ledger("AIOPSUP-1", ("AIOPSUP-5=done AIOPSUP-6=done | ", 1))
        self.assertIsNone(chat.dispatcher_parent("s1"))
        # The newer of printed and paused counts.
        self.ledger("AIOPSUP-1", ("AIOPSUP-5=done AIOPSUP-6=done | ", 1), ("6=hold | fixcap", 2))
        self.assertEqual(chat.dispatcher_parent("s1"), "AIOPSUP-1")
        self.ledger("AIOPSUP-1", ("AIOPSUP-5=done AIOPSUP-6=merged/deploying |", 1))
        self.assertEqual(chat.dispatcher_parent("s1"), "AIOPSUP-1")
        self.ledger("AIOPSUP-1")
        self.assertEqual(chat.dispatcher_parent("s1"), "AIOPSUP-1")


if __name__ == "__main__":
    unittest.main()
