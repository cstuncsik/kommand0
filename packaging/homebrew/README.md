# Homebrew tap

`brew install cstuncsik/tap/kommand0` is served from a separate **tap repo**
(`cstuncsik/homebrew-tap`). The release workflow regenerates the formula on every
release (`scripts/render-homebrew-formula.sh`) and pushes it there — but that push
needs a token, so it's **opt-in**: the `update-homebrew` job no-ops until you do
the one-time setup below.

## One-time setup

1. **Create the tap repo.** A public repo named exactly `homebrew-tap` under your
   account — the `homebrew-` prefix is what makes `cstuncsik/tap` resolve:

   ```sh
   gh repo create cstuncsik/homebrew-tap --public \
     --description "Homebrew tap for kommand0"
   ```

   It can start empty; the workflow creates `Formula/kommand0.rb` on the next
   release.

2. **Create a token that can push to it.** A fine-grained PAT scoped to
   `cstuncsik/homebrew-tap` with **Contents: read and write** (or a classic PAT
   with the `repo` scope).

3. **Add it as a secret on this repo.** In `cstuncsik/kommand0` →
   Settings → Secrets and variables → Actions → New repository secret:

   - Name: `HOMEBREW_TAP_TOKEN`
   - Value: the PAT from step 2

The next release (or a re-run of the `update-homebrew` job) will render the
formula and push it. After that:

```sh
brew install cstuncsik/tap/kommand0
# or
brew tap cstuncsik/tap && brew install kommand0
```

## How it works

- `scripts/render-homebrew-formula.sh <version> <macos_sha256> <linux_sha256>`
  prints the formula. The macOS download is the universal archive (so the
  `on_arm`/`on_intel` blocks point at the same file); Linux is `x86_64`.
- The `update-homebrew` job (in `.github/workflows/release.yml`) runs after both
  platform archives are published, pulls their `.sha256` files from the release,
  renders the formula, and commits it to the tap repo. It's idempotent — no
  commit if the formula is unchanged.

## Bootstrapping by hand (optional)

To populate the tap before the first automated release, render the current
release's formula locally and commit it to the tap repo's `Formula/` dir:

```sh
base="https://github.com/cstuncsik/kommand0/releases/latest/download"
ver=$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
  https://github.com/cstuncsik/kommand0/releases/latest | sed 's#.*/v##')
mac=$(curl -fsSL "$base/kommand0-v$ver-macos-universal.tar.gz.sha256" | cut -d' ' -f1)
lin=$(curl -fsSL "$base/kommand0-v$ver-linux-x86_64.tar.gz.sha256" | cut -d' ' -f1)
scripts/render-homebrew-formula.sh "$ver" "$mac" "$lin" > Formula/kommand0.rb
```
