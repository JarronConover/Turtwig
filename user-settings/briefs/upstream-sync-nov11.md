# Pull TJ's latest a.box fixes into Turtwig (2026-11-11)

TJ pushed stability and performance fixes to `upstream` (git@gitlab.com:tj15/a.box.git, `main`).
`origin/main..upstream/main` is 19 commits (merge-base 2d0bd6b), including:

- a795722 Fix the crash from reusing regions after a frame reused the workspace
- 653003d Scrolling a chat redraws only the chat, at 120 frames a second
- 63aa001 The box stops relaunching chats it can't keep alive
- fb1045a Moves read the source's live sidebar, time out fetches, and run one at a time
- 7a05cbe / 7dc2e2f / 5cc2dae / ad058a1 state-repo pruning, branch-only fetches, one git gc at a time
- 1f1c5e8, c7684d4, ca95606, 6760faa, 007f41a, 0f60eea, a7f3446, 59e2876 smaller fixes
- 00ec78c / d4ccdff / b0c9b9f the phone web app

## Do

1. Merge all of `upstream/main` into Turtwig `main` (use `bin/sync-upstream`, or the same steps by hand).
   Keep the fork's own changes (name, icon, bundle, sync job, user-settings/).
2. Then bring the same fixes into the open feature branches the running app is built from
   (`jarron/sdk-chat-backend`, `jarron/table-scroll`, `jarron/terminal-tiles`, `jarron/redo-agent-tiles`):
   rebase or merge main into each, resolve conflicts, keep both sides' behavior.
3. Build, run the tests, and relaunch the app on the result. Pay special attention to a795722
   (region reuse crash) and 653003d (chat scroll redraw): our settings use many `region: true` tiles.
4. Push `main` and the updated branches to origin.
5. Reply with: what merged cleanly, any conflicts and how you resolved them, test results, and
   anything from upstream you skipped and why.
