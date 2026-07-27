# Krusty extension for Zed

Runs `krusty-lsp` as the language server for Kotlin buffers in [Zed](https://zed.dev).

Zed cannot launch an arbitrary language server from `settings.json`, and the official Kotlin
extension hard-codes the binaries it downloads, so a small extension is required. This one adds a
second server (`krusty-lsp`) for the `Kotlin` language; the grammar and syntax queries still come
from the official Kotlin extension.

## Install

No build step. The extension downloads the prebuilt `krusty-lsp` for your platform automatically.

1. Install the **Kotlin** extension from Zed's extension gallery (it provides the tree-sitter
   grammar).

2. Install the **Krusty** extension from Zed's extension gallery.

3. Point Zed's `Kotlin` language at `krusty-lsp` and disable the other Kotlin servers in
   `settings.json`:

   ```json
   {
     "languages": {
       "Kotlin": {
         "language_servers": ["krusty-lsp", "!kotlin-lsp", "!kotlin-language-server", "..."]
       }
     }
   }
   ```

Open a Kotlin file. On first launch the extension shows *Checking for updates* then *Downloading*
in the status bar while it fetches the server; subsequent launches reuse the cached binary.

## Staying up to date

The extension checks the latest [GitHub release](https://github.com/qnox/krusty/releases) **every
time the language server starts** and downloads a newer build when one exists. An up-to-date launch
costs a single version check with no download.

Zed extensions are event-driven and cannot poll on a timer, so a long-lived session that never
restarts the server will not pick up a new release on its own. To refresh mid-session run
`editor: restart language server` from the command palette (or reopen the project).

## Local development override

To run your own build instead of the downloaded one — e.g. when hacking on the compiler — build the
server and point Zed at it:

```sh
cargo build --release -p krusty-lsp
```

```json
{
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

A `binary.path` here, or any `krusty-lsp` found on `PATH`, takes precedence and is used as-is — the
extension never overwrites a binary you provide. Omit `binary.path` and drop `krusty-lsp` on `PATH`
to use your own without editing settings.

To iterate on the extension itself, install this directory as a dev extension: command palette →
`zed: install dev extension` → select `editors/zed`. Zed compiles it to wasm; after editing, run
`zed: reload extensions`.

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
