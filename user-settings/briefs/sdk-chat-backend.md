# Brief: run Turtwig chats through Claude Code's SDK mode, like the VS Code extension

## Goal

Chats in Turtwig should behave exactly like Claude Code in VS Code. Today they don't, because Turtwig drives
the interactive TUI inside tmux and scrapes the screen. Switch the chat backend to Claude Code's structured
SDK protocol, which the VS Code extension uses, so every message, tool call, permission request, plan approval
and model or mode change is exchanged as data instead of keystrokes.

Repo: `~/dev/turtwig` (Jarron's fork of a.box). Don't touch `~/.agent-box/state/settings/settings.jsonc`;
the designer owns it.

## What VS Code runs (observed on this Mac with `ps`)

```
claude --output-format stream-json --verbose --input-format stream-json --max-thinking-tokens=31999
       --permission-prompt-tool=stdio --setting-sources=user,project,local --permission-mode=auto
       --include-partial-messages --replay-user-messages [--resume=<session>]
```

Read the Agent SDK / headless docs for the exact message schema (user and assistant messages, `tool_use` and
`tool_result`, `system` init, `result`, `stream_event` partials, and the control requests and responses used by
`--permission-prompt-tool=stdio`: `can_use_tool` and its allow or deny reply, `set_permission_mode`,
`set_model`, `interrupt`).

## What Turtwig does today and why it breaks

- `bin/chat` launches `claude --session-id … --remote-control … --settings {"showThinkingSummaries":true}` in a
  tmux session. The daemon (`rust/daemon/src/model.rs`) infers working, waiting and idle from hooks plus pane
  captures (`status_pattern`, `waiting_pattern`). Messages are sent with `tmux send-keys`, and answers are
  number or arrow key presses.
- Symptoms the user hit:
  - Permission buttons typed "1" into the input box when the prompt was an arrow-key menu.
  - `/model` stopped on a "keep as default?" follow-up.
  - Mode (auto, plan, manual) is read from the footer text.
  - "Waiting" outlived prompts answered in the terminal.
  - The pause button relies on Escape keystrokes.
- The designer papered over some of this in `~/.agent-box/state/settings/scripts/composer_tools.rs` (menu
  scraping, `chat.menu_pick`, `chat.mode`). Those workarounds can go once the backend is structured.

## Required behavior

1. **One long-lived `claude` process per open chat in SDK mode** with the flags above. Resume by session id.
   Keep the process off tmux or in a headless supervisor. The existing tab, close and resume semantics stay:
   a chat is "open" while its process runs.
2. **Send** through stdin as a user message (attachments as content blocks). Queued messages and `interrupt`
   use control requests, not keys.
3. **State** comes from the stream: working between a user message and `result`, waiting while a
   `can_use_tool` or AskUserQuestion request is pending, idle after `result`. Drop pane scraping for SDK chats.
4. **Permissions and questions** render from the request payload (tool, input, suggested options), and the
   card's buttons send the structured allow, deny or answer. This replaces `waiting_options` and key presses.
5. **Model, effort and mode:** the message box's model picker, effort slider and mode menu (Manual, Edit
   automatically, Plan, Auto) call `set_model`, `set_permission_mode` and the effort setting directly, and
   reflect the current value from the stream. No `/model` typing and no Shift+Tab cycling.
6. **Transcript:** keep reading the session JSONL for history, but render live turns from the stream,
   including partial text and thinking, so replies appear as they're written, like VS Code.
7. **Keep the remote-control and hook features** that still matter (inbox, notifications, sync-worker on Stop).
   Verify each still fires in SDK mode or replace it with the stream equivalent.
8. **Keep Codex on the existing TUI path.** Gate the new path per agent, e.g. `daemon.agents.claude.transport:
   "sdk"`, defaulting to sdk for Claude.

## Acceptance, checked by hand in Turtwig

- Start a chat. Ask for something that needs a Bash permission in Manual mode. The card shows the real
  options, and Allow runs the tool, with nothing typed into the input.
- Switching the model in the picker takes effect on the next turn without any follow-up prompt, and the pill
  shows the new model.
- Switching to Plan makes the agent plan, and approving the plan continues it.
- The pause button stops a running turn within a second and the chat stays open.
- An AskUserQuestion with several options is answerable from the card.
- Closing and reopening a tab resumes the same session.
- Streaming text appears token by token.

Work on a branch, commit in small steps, and report what's verified against each acceptance line.
