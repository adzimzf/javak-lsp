# javak-lsp
A Java Kotlin LSP

## Layout

Each layer keeps what is particular to a language in a `java` and a `kotlin`
module behind an interface of its own:

| Layer | Interface | Per language |
| --- | --- | --- |
| `src/parser` | `Syntax` | parsing, outline, folds; Kotlin is lowered to the Java-shaped tree the features read |
| `src/feature` | `LanguageFeatures` | how signatures are written, keywords, what a reference includes |
| `src/indexer` | `SourceLayout` | source roots, what a library source file declares |
| `src/backend` | `LanguageProtocol` | the protocol's language ids |

## Libraries

On startup the server asks the project's own build tool (Gradle or Maven) for
its dependencies and their sources jars (`src/dependency_sync`), and resolves
names into those sources: go to definition opens the library file, extracted
under `~/.cache/javak-lsp/sources`. The sync runs again when a build file
changes. Libraries without a sources jar are decompiled with Vineflower.

Client settings (`initializationOptions`):

```json
{
  "dependencySync": "auto",
  "indexingOptions": { "decompileLibraries": true }
}
```

`"dependencySync": "off"` never runs the build tool; the libraries of the last
sync are still read.

## Tests

```sh
cargo test                                   # unit tests
INTEGRATION=1 cargo test --lib integrationtest   # every case under src/integrationtest/cases
```
