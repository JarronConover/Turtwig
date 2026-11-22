# Turtwig's baseline settings

Jarron's `~/.agent-box/state/settings`: `settings.jsonc` (overrides on top of `rust/app/default-settings.jsonc`),
`scripts/` (Rust scripts the app compiles) and `ui-state.json` (tiles, notes, picks). `briefs/` holds task briefs
for the Turtwig agent and isn't seeded.

The app embeds the first three at build time (`rust/app/build.rs`, `rust/app/src/baseline.rs`) and seeds
them into the settings folder on launch when a file is missing; it never overwrites one that exists.
"Reset to Turtwig baseline" in the palette backs the folder up to `settings.bak-<stamp>/` and seeds again.

Keep these files portable: `~` for the home folder (the app expands it), no chat ids, worktree paths or
shell ids in `ui-state.json` (the seeder drops them anyway, see `baseline::portable_state`).
