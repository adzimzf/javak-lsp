---
name: comments
description: Rules for writing code comments and README text. Use whenever adding or editing comments, doc comments, or README.md.
---

# Comments and README

Do not add comments that restate what the code does, narrate changes
("added X", "fixed Y"), or describe the task. Do not add README sections for
internal details.

Add a comment only when:
- the item is exposed to other modules (public API: `pub` items, LSP handlers,
  sidecar protocol), to document its contract, or
- there is a non-obvious reason for a decision (matching a jdtls quirk, a
  performance or memory trade-off, a workaround) that the code cannot show.
