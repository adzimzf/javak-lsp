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

## The workspace

Like jdtls, the server reads the whole project, not only the files the
editor has open: every `.java` and `.kt` file below the workspace root
(build output, `.git` and other hidden directories aside) is read at
`initialize`, so a name resolves into a file nobody has opened, and its
references and renames reach it. An open file is the editor's text; the
others are read again when the client reports them changed
(`workspace/didChangeWatchedFiles`) or saved.

Beside its syntax errors and a missing `package`, a Java file is told what
jdtls would tell it where the server can be sure: a feature its build
file's source level does not have (a text block below 15), and a call no
method of its name takes (`parse(test)` of `parse(String, Parser)`).

## Libraries

On startup the server asks the project's own build tool (Gradle or Maven) for
its dependencies and their sources jars (`src/dependency_sync`), and resolves
names into those sources: go to definition opens the library file, extracted
under `~/.cache/javak-lsp/sources`. The sync runs again when a build file
changes. Libraries without a sources jar are decompiled with Vineflower.

The platform is read the same way, build tool or none
(`src/dependency_sync/platform.rs`):

- **JDK** — `src.zip` of the JDK named by `javaHome`, else `$JAVA_HOME`, the
  `java` on the `PATH`, the login shell's `JAVA_HOME`, then the usual install
  locations (`/usr/lib/jvm`, `/Library/Java/JavaVirtualMachines`, SDKMAN,
  Gradle toolchains, …). A JDK without `src.zip` (a JRE, or Debian's
  `openjdk-*-jdk` without `openjdk-*-source`) is reported in the log.
- **Kotlin standard library** — for a project with Kotlin sources, the
  `kotlin-stdlib` sources jar the build tool resolved, else the one of the
  version the project depends on (or the newest) in the Gradle or Maven cache,
  else the `kotlinc` install's.

Names resolve as each language sees them: `java.lang.*` is imported in every
Java file, Kotlin's default imports (`kotlin.*`, `kotlin.collections.*`,
`kotlin.io.*`, `kotlin.text.*`, …) in every Kotlin file, so a Kotlin `String`
is `kotlin.String`. Kotlin top-level functions (`println`, `listOf`) and
extensions (`list.first()`, `text.uppercase()`) are found by their receiver's
type and its supertypes, the JVM's `actual` declarations before the common
`expect` ones. A library file is parsed when an answer first needs it — the
`PrintStream` of `System.out` — and kept for the rest of the session.

Client settings (`initializationOptions`):

```json
{
  "dependencySync": "auto",
  "javaHome": "/path/to/jdk",
  "indexingOptions": { "decompileLibraries": true }
}
```

`"dependencySync": "off"` never runs the build tool; the libraries of the last
sync, and the platform's sources, are still read.

## Tests

```sh
cargo test                                   # unit tests
INTEGRATION=1 cargo test --lib integrationtest   # every case under src/integrationtest/cases
```
