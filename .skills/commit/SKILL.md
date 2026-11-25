---
name: commit
description: Commit work in small, frequent steps with messages that explain the background. Use after every self-contained change (a fix, a feature slice, a test, a refactor) instead of batching work into one large commit, and whenever writing code comments or README text.
---

# Commit often, explain why

## When to commit

Commit as soon as a change is self-contained and the build passes:
- one behavior added or fixed, with its tests
- one refactor that does not change behavior
- one benchmark, workflow or tooling change

Do not wait for a whole task to finish. Do not mix unrelated changes in one
commit; stage only the paths that belong to it (`git add <path>`).

Before each commit:
1. `cargo build` and `cargo test` (or the narrower test for what changed) pass.
2. `git diff --staged` contains only this change.
3. If not on a feature branch, create one first; never commit to `main`.

## Message

```
<imperative summary, under 72 chars>

<Background: why this change is needed — the problem, the jdtls/KotlinLSP
behavior being matched, the bug report, or the measurement that motivated it.>

<What changed, in terms of observable behavior, not a file list.>

<Compatibility, speed and memory impact when relevant: does it match
jdtls/KotlinLSP, is it slower, does it use more memory.>
```

Every commit body must state the background. A message with only a summary
line is not acceptable, even for small changes.

## Comments and README

Do not add comments that restate what the code does, narrate changes
("added X", "fixed Y"), or describe the task. Do not add README sections for
internal details.

Add a comment only when:
- the item is exposed to other modules (public API: `pub` items, LSP handlers,
  sidecar protocol), to document its contract, or
- there is a non-obvious reason for a decision (matching a jdtls quirk, a
  performance or memory trade-off, a workaround) that the code cannot show.
