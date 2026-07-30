# kommand0 screencast — generate with:  ./demo/setup.sh && vhs demo/demo.tape
# (run from the repo root). Produces demo/demo.gif.

Output demo/demo.gif

Set Shell "bash"
Set FontSize 16
Set Width 1100
Set Height 660
Set Padding 18
Set Theme "Catppuccin Mocha"
Set TypingSpeed 55ms
Set WindowBar Colorful
Set BorderRadius 8
Set CursorBlink false

# --- setup (hidden): point kommand0 at the seeded demo env + stub claude ---
Hide
Type `export KOMMAND0_STATE_DIR="$PWD/demo/.work/state" KOMMAND0_CLAUDE_BIN="$PWD/demo/claude-demo"`
Enter
Type "clear"
Enter
Show

# --- launch ---
Type `"$PWD/target/release/kommand0"`
Enter
Sleep 2.5

# Expand the repo to reveal its workspaces (one shows an uncommitted-changes *).
Type "l"
Sleep 1.5
Down
Sleep 0.8
Down
Sleep 1.5

# Command palette: fuzzy-jump to a workspace and open its embedded Claude.
Type ":"
Sleep 0.8
Type "auth"
Sleep 1.5
Enter
Sleep 2.5

# Open a second session tab on the same workspace, then switch back.
Ctrl+A
Type "c"
Sleep 2.5
Ctrl+A
Type "["
Sleep 1.5

# Back to the tree.
Ctrl+A
Type "t"
Sleep 1.2

# Command palette again — actions too (here: narrow to archiving a workspace).
Type ":"
Sleep 0.8
Type "archive search"
Sleep 2
Escape
Sleep 1

# Quit.
Type "q"
Sleep 1.2
