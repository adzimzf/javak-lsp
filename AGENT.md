# javak-lsp

javak-lsp is a language server for Java and Kotlin with full interop
between the two. It aims to be faster and to use less memory than running
JDTLS and the Kotlin LSP side by side.

## Before every change, ask

1. **Compatibility:** does it behave the way JDTLS (for Java) and the
   Kotlin LSP (for Kotlin) do?
2. **Speed:** does it make anything slower?
3. **Memory:** does it use more memory?

A change that fails any of these needs a clear reason to go in.
