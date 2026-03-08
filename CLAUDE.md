## Default working style

For non-trivial work, use this sequence by default:

1. Discuss
2. Research
3. Plan
4. Execute
5. Verify

Do not jump straight into implementation unless the task is obviously tiny.

## Discuss

Start by clarifying:
- the goal
- constraints
- what should not change
- whether the task is tiny, normal, or risky

For tiny tasks, keep this brief.
For larger tasks, restate the intended outcome before proceeding.

## Research

Before planning or editing:
- inspect the relevant files
- inspect neighboring patterns
- identify affected modules, commands, tests, and docs
- note risks, assumptions, and unknowns

Use the researcher subagent when the task is medium or large.

## Plan

Before editing, present:
- files likely to change
- step-by-step approach
- risks
- validation steps

Keep plans proportionate to task size.
Do not over-plan tiny fixes.

Use the planner subagent for non-trivial work.

## Clarification rule

Before implementation:
- ask clarifying questions when the task is ambiguous or risky
- ask no more than 3 focused questions at once
- do not ask questions for tiny obvious edits
- ask for confirmation before broad refactors or architectural changes

## Execute

During implementation:
- make the smallest reasonable changes
- preserve existing structure unless a change is clearly justified
- avoid broad refactors unless explicitly requested
- keep code readable and practical
- keep changes aligned with the approved plan

## Verify

After implementation:
- run relevant tests/checks if available
- verify behavior manually when needed
- summarize what changed
- call out anything not verified

Use the verifier subagent for medium or large changes.

## Subagent usage rules

Use:
- researcher for exploration, codebase reading, dependency mapping, risk discovery
- planner for turning findings into a concrete implementation plan
- verifier for post-change review, validation, and missing-check detection

Do not spawn subagents for trivial single-file edits unless it adds clear value.

## Style preferences

- Prefer straightforward solutions over abstractions
- Keep the TUI thin
- Keep shared domain logic in core crates
- Avoid speculative architecture
- Preserve good names and clean boundaries
- When unsure, choose the simpler path

## Output preferences

For normal tasks:
- brief summary
- research findings
- plan
- implementation summary
- verification summary

For tiny tasks:
- keep it compact
