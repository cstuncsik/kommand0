# External Integrations

**Analysis Date:** 2026-03-07

## APIs & External Services

**None.** Kommand0 is a fully local application with no external API calls, no cloud services, and no network dependencies.

## Data Storage

**Databases:**
- None. No database engine is used.

**File Storage:**
- Local filesystem only
- State file: `.kommand0-dev/state.json` (relative to working directory)
- Managed by `AppState::load()` and `AppState::save()` in `crates/core/src/lib.rs`
- Format: Pretty-printed JSON via `serde_json`
- Schema: `{ repos: [{ id: string, name: string, path: string }] }`

**Caching:**
- None

## Authentication & Identity

**Auth Provider:**
- Not applicable. Local-only tool with no authentication.

## Monitoring & Observability

**Error Tracking:**
- None. Errors are handled via `anyhow::Result` and displayed to the user inline.

**Logs:**
- `tracing` crate is declared as a dependency in `crates/core/Cargo.toml` but not yet used in code
- No tracing subscriber configured
- No log output files or destinations configured

## CI/CD & Deployment

**Hosting:**
- Local binary distribution only (no hosted deployment)

**CI Pipeline:**
- None configured. No GitHub Actions, no CI config files detected.

## Environment Configuration

**Required env vars:**
- None

**Secrets location:**
- Not applicable. No secrets are used.

## External Process Integrations

**Git CLI:**
- The only external integration is shelling out to `git`
- Implementation: `run_git_status()` in `crates/core/src/lib.rs`
- Invocation: `git -C <repo_path> status --short --branch`
- Uses `std::process::Command` (synchronous)
- Error handling: checks `output.status.success()`, returns stderr on failure
- Future milestone (SESS-01) will add general command execution with streaming output

## Webhooks & Callbacks

**Incoming:**
- None

**Outgoing:**
- None

## Future Integration Points

Based on `gds-brief.md` roadmap:
- **Git worktree** integration planned for Milestone 5 (TREE-01)
- **General process execution** planned for Milestone 3 (SESS-01 through SESS-04) - will use tokio for async process management
- No external service integrations are planned; the tool is designed to remain local-only

---

*Integration audit: 2026-03-07*
