# Krusty extension for Zed

Runs `krusty-lsp` as the language server for Kotlin buffers in [Zed](https://zed.dev).

Zed cannot launch an arbitrary language server from `settings.json`, and the official Kotlin
extension hard-codes the binaries it downloads, so a small extension is required. This one adds a
second server (`krusty-lsp`) for the `Kotlin` language; the grammar and syntax queries still come
from the official Kotlin extension.

## Install

1. Build the server:

   ```sh
   cargo build --release -p krusty-lsp
   ```

2. Install the **Kotlin** extension from Zed's extension gallery (tree-sitter grammar).

3. Install this directory as a dev extension: command palette → `zed: install dev extension` →
   select `editors/zed`. Zed compiles it to wasm; after editing the extension, use
   `zed: reload extensions`.

4. Point Zed at the server and disable the other Kotlin servers in `settings.json`:

   ```json
   {
     "languages": {
       "Kotlin": {
         "language_servers": ["krusty-lsp", "!kotlin-lsp", "!kotlin-language-server", "..."]
       }
     },
     "lsp": {
       "krusty-lsp": {
         "binary": {
           "path": "/absolute/path/to/krusty/target/release/krusty-lsp",
           "arguments": ["--stdio"]
         }
       }
     }
   }
   ```

   Omit `binary.path` to take `krusty-lsp` from `PATH`.

## Environment

The extension passes the worktree shell environment to the server, so `JAVA_HOME` from the shell,
mise, or direnv is visible to `krusty-lsp`. Override it explicitly with `binary.env`, or pass
`-jdk-home <path>` in `binary.arguments`; without a JDK the server resolves no `java.*` symbols.

## Project model

The server detects BSP, Gradle, or Maven from the worktree and refreshes the classpath after build
file changes. A `-cp` argument disables build-tool detection and uses that classpath for the server
lifetime.

## Supported features

Diagnostics, hover, completion (with resolve), semantic highlighting, full-document sync. Requests
Zed sends for other features (go-to-definition, rename, formatting) are not implemented.
