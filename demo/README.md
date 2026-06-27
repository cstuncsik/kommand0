# Demo screencast

The GIF at the top of the main [README](../README.md) is generated with
[vhs](https://github.com/charmbracelet/vhs).

## Regenerate

From the repo root:

```sh
brew install vhs        # or: https://github.com/charmbracelet/vhs#installation
./demo/setup.sh         # build binaries + seed a demo env under demo/.work/
vhs demo/demo.tape      # records demo/demo.gif
```

Then commit the updated `demo/demo.gif`.

## What's here

- **`demo.tape`** — the vhs script: terminal settings + the scripted keystrokes
  for the recording.
- **`setup.sh`** — builds the release binaries and a self-contained demo
  environment (a git repo with `auth` / `payments` / `search` workspaces, one
  left dirty so the tree shows a `*`) under `demo/.work/` (git-ignored, rebuilt
  each run).
- **`claude-demo`** — a stub `claude` so the recording needs no auth and is
  deterministic; kommand0 launches it in the embedded pane via
  `KOMMAND0_CLAUDE_BIN`.

The tape points kommand0 at the seeded state and the stub (via
`KOMMAND0_STATE_DIR` / `KOMMAND0_CLAUDE_BIN`), so it records the real TUI without
touching your own repos or `~/.claude`.
