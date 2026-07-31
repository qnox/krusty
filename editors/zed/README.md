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

2. Install the **Krusty** extension from Zed's extension gallery. *(Gallery listing pending — until
   then, install from source; see [Until it's in the gallery](#until-its-in-the-gallery) below.)*

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

## Until it's in the gallery

Before the gallery listing lands, install the extension from source. Zed compiles it locally, so a
Rust toolchain with the wasm target is required (this is a one-time setup for the *extension*; the
`krusty-lsp` server itself is still downloaded, not built):

```sh
rustup target add wasm32-wasip1
```

1. Install the **Kotlin** extension from Zed's gallery (grammar), as above.

2. Clone this repository, then: command palette → `zed: install dev extension` → select the
   `editors/zed` directory. Zed compiles the extension to wasm (~30s the first time).

3. Apply the same `settings.json` from [Install](#install) to route `Kotlin` at `krusty-lsp`.

Open a Kotlin file; the server downloads exactly as in the gallery flow. After editing the extension
source, run `zed: reload extensions`.

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

## Developer dump

With dev mode on, `cmd-.` on a Kotlin file offers **krusty (dev): dump AST + checker + IR**, which
opens a Markdown buffer holding the file's parsed arenas, its diagnostics and inferred expression
types, and its lowered IR — or the reason lowering bailed. The dump path is stable per source file,
so a buffer left open in a split refreshes in place each time the action runs. A document is
re-rendered only once the file has been analyzed again, so repeated requests — including the code
action refreshes a client issues on every cursor settle — reuse the file already written.

Dump documents contain source identifiers and literals. Their cache filenames are therefore opaque
SHA-256 digests of the full document URI rather than readable workspace paths; on Unix, the dump
directory and files are restricted to the current user. Rendering stops at 64 MiB and appends a
truncation marker; the store retains at most 64 files / 256 MiB, evicting the oldest entries. These
limits are independent of the dependency-source cache limits because dumps and dependency stubs have
different privacy and lifetime rules.

Dev mode is off by default. Turn it on in Zed settings:

```json
{ "lsp": { "krusty-lsp": { "binary": { "arguments": ["--dev"] } } } }
```

The dump is a debugging view of internal compiler structures. Its format tracks those structures and
is not stable.

Known limitations:

- Only a module's own primary documents are dumpable. A file the latest analysis pass saw purely as
  support for a different module is not: dumping it under that module's classpath and language
  arguments would describe a compilation the editor never performed. Files from several open modules
  are each retained under their own module up to the shared 64 MiB replay-input budget; later groups
  are left undumpable rather than multiplying repeated dependency source sets without a bound.
- A dump requested while an edit is still being analyzed can replay pre-edit state. The `source
  hash` in the document's header identifies the text it was rendered from, so a stale dump is
  recognizable even when the edit preserved the file's length.
