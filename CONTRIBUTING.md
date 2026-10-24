# Contributing to krusty

Thank you for wanting to help. For now, krusty accepts **issues, not code**.

## Why no pull requests

krusty is built with coding agents. In that workflow, turning a precise, well-structured issue into
our own implementation is faster and safer than reviewing an outside change: the change has to fit
the compiler's invariants (see [`AGENTS.md`](AGENTS.md)), pass the differential harness against
kotlinc, and land with a regression test, and we would end up rewriting most contributions anyway.
A good issue gets the same result without either of us spending time on review rounds.

So please don't open a pull request with code. We will close it and ask for an issue instead. A
fix you have already written is still welcome as a sketch inside the issue.

## Let your agent write the issue

If you use a coding agent, ask it to draft the issue for you, and review it before you submit. The
issues we can act on fastest have:

- **A minimal reproduction:** the smallest Kotlin source (and build setup, if it matters) that shows
  the problem, inline in the issue.
- **The command you ran** and the krusty version (`krusty -version`) and Kotlin version.
- **Expected vs. actual:** what kotlinc does with the same input and what krusty does instead. kotlinc
  is the reference, so its output is the expected result: diagnostics, program output, or a
  `javap -c -p -v` diff of the class files.
- **Nothing private:** strip names, paths and code you cannot share.

A prompt that works well: *"Reduce this krusty problem to a minimal standalone reproduction, compile
it with both kotlinc and krusty, and write a GitHub issue with the source, the commands, and the
expected (kotlinc) vs. actual (krusty) result."*

## Verify reports

If you build with Gradle or Maven, verify mode (`./gradlew krustyVerify`, or the Maven plugin's
[`verify` goal](tools/krusty-maven/README.md)) compiles your build with both kotlinc and krusty and
reports what differs, without changing what the build ships. Its report has a button that opens a
prefilled
[verify report issue](https://github.com/qnox/krusty/issues/new?template=verify-report.md); review it
before you submit. That alone tells us which gaps matter to real builds.

## Other ways to help

- Feature requests and questions are welcome as issues too.
- If krusty saves you build time, consider [sponsoring](https://github.com/sponsors/qnox) the work.
