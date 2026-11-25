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

## Examples

### Bad: restates the code

```rust
// Check if the path is a build file
if is_build_file(path) {
    // Reload the dependencies
    self.reload(path);
}

let mut count = 0; // initialize count to zero
```

The names already say this. Delete the comments.

### Bad: narrates the change or the task

```rust
// Added to fix folding bug reported in #14
// TODO(agent): updated this per user request
// New implementation using tree-sitter instead of regex
fn fold_ranges(tree: &Tree) -> Vec<FoldingRange> { ... }
```

History belongs in the commit message, not the code. Delete them.

### Bad: doc comment on a private helper that only repeats its signature

```rust
/// Returns the label for the given node.
///
/// # Arguments
/// * `node` - the node
fn label_of(node: Node<'_>) -> String { ... }
```

Private and obvious from the name. No comment.

### Good: contract of an item exposed to other modules

```rust
/// Returns the call hierarchy item for the symbol at `at`, or `None` when
/// the position is not on a method, constructor or type declaration or use.
/// The item's range spans the whole declaration; `selection_range` is the name.
pub fn item_at<'a>(snapshot: &'a Snapshot, uri: &Url, at: Position) -> Option<Item<'a>> { ... }
```

Callers in other modules need to know when it returns `None` and what the
ranges cover; the signature cannot tell them.

### Good: reason for matching a jdtls quirk

```rust
// jdtls starts a method's fold at the line of its name, not its annotations,
// so clients collapse annotations together with the signature.
let start = method.name_node().start_position().row;
```

Without the comment, someone would "fix" this to start at the annotations
and break compatibility.

### Good: performance or memory trade-off

```rust
// Interned once per file: completion asks for the same simple names
// thousands of times, and owned Strings doubled peak memory on large projects.
let name = interner.intern(ident);
```

Explains why the simpler code was not used, with the measured cost.

### Good: workaround with a non-obvious cause

```rust
// The sidecar may answer before the file is saved to disk; resend the
// current text so it never compiles a stale version.
sidecar.update(uri, &doc.text)?;
```

## README

Bad: an "Implementation notes" section describing internal modules, or a
changelog of what was fixed. Good: what a user needs to install, configure
and run the server, and how it differs from jdtls/KotlinLSP in behavior.
