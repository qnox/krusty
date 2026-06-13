# Contributor & assistant guidelines for krusty

## Branding policy — NO AI/tool attribution (hard rule)

This project does **not** carry any AI, assistant, or tool branding. When working on this repo
(human or AI assistant), you MUST NOT add:

- `Co-Authored-By:` trailers naming an AI/assistant/tool (e.g. Claude, Copilot, GPT, etc.)
- "Generated with", "Created by", "🤖", or similar attribution in commit messages, PR bodies,
  code comments, or docs
- Tool/vendor names in author/committer fields

All commits are authored by the project maintainer. Commit messages describe the change only —
what and why — with no tooling provenance. Keep this rule when amending or rewriting history.

## Engineering conventions

- **TDD is required.** Every feature lands with a test; every phase ends on a green `cargo test`.
- The AST/IR stays **index-based** (`u32` ids into parallel `Vec`s — no `Box`/`Rc` graphs).
- Correctness is defined by the **differential harness** vs the real `kotlinc`: don't claim a
  feature works without an ABI-signature diff and/or a round-trip test.
- Record every Kotlin-semantics decision in `docs/SPEC.md` with a test.
- Keep `docs/SPEC.md`, `docs/IMPLEMENTATION_PLAN.md`, and `docs/METADATA_NOTES.md` current.
