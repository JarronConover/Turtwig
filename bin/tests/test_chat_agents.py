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
                         ["claude", "--session-id", "c1", "--remote-control", "n", "--model", "opus", "go"])
        self.assertEqual(chat.fill(chat.AGENTS["claude"]["resume"], dict(v, model=None)),
                         ["claude", "--resume", "c1", "--remote-control", "n"])
        self.assertEqual(chat.fill(chat.AGENTS["claude"]["fork"], v),
                         ["claude", "--resume", "s0", "--fork-session", "--session-id", "c1", "--remote-control", "n",
                          "--model", "opus"])

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


if __name__ == "__main__":
    unittest.main()
