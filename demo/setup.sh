#!/bin/sh
# Build the binaries and a self-contained demo environment for the vhs
# screencast: a sample git repo with a few kommand0 workspaces (one left dirty
# so the tree shows a `*`). Run once from the repo root before recording:
#
#   ./demo/setup.sh && vhs demo/demo.tape
#
# Everything lives under demo/.work/ (git-ignored) and is rebuilt each run.
set -eu

REPO=$(cd "$(dirname "$0")/.." && pwd)
WORK="$REPO/demo/.work"
KMD="$REPO/target/release/kmd"

echo "building release binaries…" >&2
cargo build --release --quiet --manifest-path "$REPO/Cargo.toml" --bin kommand0 --bin kmd >&2

rm -rf "$WORK"
mkdir -p "$WORK"

# A sample repo with a little history.
app="$WORK/webapp"
mkdir -p "$app"
git -C "$app" init -q -b main
git -C "$app" config user.email demo@kommand0
git -C "$app" config user.name "kommand0 demo"
printf 'fn main() {\n    println!("hello");\n}\n' >"$app/main.rs"
git -C "$app" add -A
git -C "$app" -c commit.gpgsign=false commit -qm "init webapp"

# Build the demo state via the CLI so the worktrees/branches/state.json are real.
export KOMMAND0_STATE_DIR="$WORK/state"
"$KMD" repo add "$app" >/dev/null
for ws in auth payments search; do
	"$KMD" workspace create "$ws" --repo webapp >/dev/null
done

# Leave one workspace dirty so the demo shows an uncommitted-changes marker.
# Worktrees nest under the repo id (worktrees/<repo-id>/<name>); a glob in a
# redirection word is NOT expanded by sh, so loop instead.
for f in "$WORK"/state/worktrees/*/auth/main.rs; do
	echo '    // wip: rate limiter' >>"$f"
done

echo "demo env ready at $WORK" >&2
echo "now record with:  vhs demo/demo.tape" >&2
