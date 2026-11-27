# javak-lsp
A Java Kotlin LSP

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/adzimzf/javak-lsp/main/scripts/install.sh | sh
```

This installs the newest release, release candidates included, as
`~/.local/bin/javak-lsp`, with `javak-debug-server.jar` in
`~/.local/share/javak-lsp`. Run it again to update. `--stable` skips release
candidates, `--version v0.1.0-rc.3` picks one, and `--prefix /usr/local`
installs elsewhere (Linux and macOS, x86_64 and arm64). Every pull request
merged into `main` publishes a release candidate
(`.github/workflows/release-candidate.yml`).

## Layout

Each layer keeps what is particular to a language in a `java` and a `kotlin`
module behind an interface of its own:

| Layer | Interface | Per language |
| --- | --- | --- |
| `src/parser` | `Syntax` | parsing, outline, folds; Kotlin is lowered to the Java-shaped tree the features read (Java's outline is jdtls's, built in `src/feature/outline.rs`; its folds are jdtls's, in `src/parser/java/folds.rs`) |
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

A workspace holds as many projects as its build files say, as jdtls
imports them (`src/indexer/project.rs`): every Maven and Gradle build up to
five directories down, every module a `pom.xml` lists (`<modules>`, those
of profiles too) and every project a `settings.gradle` includes
(`include`, `includeFlat`, `projectDir`, `includeBuild`). Each module has
its own source roots, `src/main/java` and `src/test/java` unless its build
says otherwise (`<sourceDirectory>`), and its own Java level, a Maven module
inheriting its parent's. Gradle's own answer replaces what its files
suggest once the dependency sync has asked it: the source directories of
each project's Java, Kotlin and Android source sets, and the level it
compiles at. A workspace without a build file is one project at its root.

Beside its syntax errors and a missing `package`, a Java file is told what
jdtls would tell it where the server can be sure: a feature its module's
source level does not have (a text block below 15), a call no method of its
name takes (`parse(test)` of `parse(String, Parser)`), and what the Eclipse
compiler says of the file's own text (task tags, duplicate methods, dead
code, unused private fields and imports, …). A file on no source root of
any module is read for its syntax and task tags only.

## Libraries

On startup the server asks the project's own build tool (Gradle or Maven) for
its dependencies and their sources jars (`src/dependency_sync`), and resolves
names into those sources: go to definition opens the library file, extracted
under `~/.cache/javak-lsp/sources`. The sync runs again when a build file
changes.

A library without a sources jar (or classes its sources jar lacks) is read
as jdtls and IntelliJ read it, with `decompileLibraries` (below):

- **`"onDemand"`** (the default) — the declarations are read from its class
  files, in the server, without a JVM (`src/classfile.rs`,
  `src/dependency_sync/stubs.rs`): types, generic signatures, constants,
  parameter names where the class file kept them, annotations. Hover,
  completion, types and checks answer from them at once (guava's 2,000
  classes in 0.2 s). Go to definition decompiles the one class it opens with
  Vineflower, about a second and a half the first time, shown as work in
  progress, then kept. A Kotlin library is decompiled whole, in the
  background, as a Java view of it would mislead Kotlin callers.
- **`"eager"`** — every such library is decompiled once, in the background,
  so the bodies are read too (find references inside them).
- **`"off"`** — neither: such classes are unknown.

The decompiler runs on the user's `java` (one thread, the client compiler,
the serial collector), is fetched through the build tool, and what it writes
is kept under `~/.cache/javak-lsp/decompiled`, once per jar version.

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
  "indexingOptions": { "decompileLibraries": "onDemand" }
}
```

`decompileLibraries` is `"onDemand"`, `"eager"` or `"off"`; `true` is
`"onDemand"` and `false` is `"off"`.

`"dependencySync": "off"` never runs the build tool; the libraries of the last
sync, and the platform's sources, are still read.

## Formatting

A Java file is formatted (`textDocument/formatting`, `rangeFormatting`) as
jdtls formats it with its default settings, Eclipse's built-in profile
(`src/feature/format.rs`): one statement a line, braces at the end of the
line, the line breaks the source has where a line may wrap, lines wrapped at
120 columns and comments at 80. The indentation is the client's (`tabSize`,
`insertSpaces`). A member that does not parse is left as it is, and so is
anything between `@formatter:off` and `@formatter:on`. Kotlin files are not
formatted.

## Debugging

javak-lsp answers the `workspace/executeCommand` commands of jdtls' debug
plugin (`com.microsoft.java.debug.plugin`), so an editor set up for jdtls'
debugger (nvim-dap with nvim-jdtls, Zed, vscode-java-debug) debugs through
javak-lsp unchanged:

| Command | Answer |
| --- | --- |
| `vscode.java.startDebugSession` | the port of a Debug Adapter Protocol server: Microsoft's java-debug, the adapter jdtls runs |
| `vscode.java.resolveMainClass` | every `static void main(String[])`, Kotlin `fun main` and `@JvmStatic main`: `[{mainClass, projectName, filePath}]` |
| `vscode.java.resolveMainMethod` | a file's `main` methods, for the Run / Debug code lens |
| `vscode.java.resolveClasspath` | `[modulePaths, classPaths]`: the build modules' compiled classes and the jars of the last dependency sync |
| `vscode.java.resolveJavaExecutable` | the `java` of `javaHome`, `$JAVA_HOME` or the `PATH` |
| `vscode.java.buildWorkspace` | the project compiled by its own build tool (`gradle classes testClasses`, `mvn test-compile`) |
| `vscode.java.updateDebugSettings`, `validateLaunchConfig`, `checkProjectSettings`, `isOnClasspath`, `fetchPlatformSettings`, `inferLaunchCommandLength`, `resolveSourceUri`, `fetchUsageData` | as jdtls answers them |

Inside a session java-debug asks javak-lsp what jdtls answers from its
Java model: the class that holds a breakpoint's line (the outermost class,
or a Kotlin file's facade, `MainKt`) and the file a stack frame's class came
from, workspace or library sources (`src/debugger`). Expressions — watches,
the debug console, breakpoint conditions, logpoints — are interpreted over
JDI: names, `this`, fields, array elements, method calls, literals and Java's
operators. jdtls compiles them with JDT, so it takes any Java; javak-lsp
does not do hot code replace or step-into-target, and needs the code built
(`buildWorkspace`, or the build tool) before a launch.

java-debug runs in `javak-debug-server.jar`, built from
`java-sidecar/debug-server`:

```sh
cd java-sidecar/debug-server && mvn -q package   # target/javak-debug-server.jar
```

The server looks for it at `$JAVAK_LSP_DEBUG_SERVER_JAR`, next to the
`javak-lsp` binary, in `../share/javak-lsp`, in `~/.cargo/bin`, and in this
checkout's `java-sidecar/debug-server/target`. With nvim-dap alone:

```lua
dap.adapters.java = function(callback)
  local client = vim.lsp.get_clients({ name = "javak-lsp" })[1]
  client:request("workspace/executeCommand", { command = "vscode.java.startDebugSession" },
    function(_, port) callback({ type = "server", host = "127.0.0.1", port = port }) end)
end
```

`python3 java-sidecar/debug-server/e2e_test.py` drives a whole session
(launch, breakpoints, a condition and a logpoint, the stack, variables,
expressions) against a debug build.

## Tests

```sh
cargo test                                   # unit tests
INTEGRATION=1 cargo test --lib integrationtest   # every case under src/integrationtest/cases
just benchmark                                  # javak-lsp vs jdtls on real projects
just benchmark jsoup                            # one project
just benchmark "" random                        # random questions; the seed is printed
just benchmark jsoup 42                         # replay seed 42 exactly
just benchmark-quick                            # reuse jdtls's recorded answers
```

`just benchmark` clones the projects listed in
`src/integrationtest/benchmark/catalog.rs`, fetches their libraries with Maven
or Gradle, asks jdtls questions chosen from their code (navigation, hover,
references, rename, hierarchies, outline, folding, semantic tokens,
formatting, and completion and diagnostics after an edit), asks javak-lsp
the same about a clean copy, and shows only the answers that differ.
Recordings and `report.md` are written to `target/jdtls-compat`. jdtls is
`$JDTLS_CMD`, the `jdtls` on the `PATH`, or a milestone downloaded into
`~/.cache/javak-lsp/jdtls`. The `jdtls compat` workflow runs the same on
every pull request and keeps one comment on it up to date with the result;
the variables it reads are listed in `src/integrationtest/benchmark/mod.rs`.
