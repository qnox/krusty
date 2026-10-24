---
name: Bug report
about: krusty rejects valid code, accepts invalid code, crashes, or produces output that differs from kotlinc
title: ""
labels: bug
---

<!--
Thanks for the report. krusty takes issues rather than code (see CONTRIBUTING.md), so a precise
issue is the best contribution. If you use a coding agent, ask it to reduce the problem and draft
this issue for you, then review it before submitting. Leave out anything you cannot share.
-->

### Reproduction

```kotlin
// The smallest source that shows the problem.
```

Command, krusty version (`krusty -version`) and Kotlin version:

```sh
```

### Expected (kotlinc)

<!-- What kotlinc does with the same input: diagnostics, program output, or class files. -->

### Actual (krusty)

<!-- What krusty does instead. For class file differences, a `javap -c -p -v` diff helps most. -->
