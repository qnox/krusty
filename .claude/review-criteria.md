1. Correctness — the emitted bytecode/ABI matches kotlinc; edge cases in erasure, boxing, mangling, and nullability are handled; no miscompilation
2. Code quality — clean Rust, proper `Result`/`Option` handling, no panics on reachable paths, readable control flow
3. Performance — no needless allocations/clones, efficient IR traversal, no accidental quadratic passes
4. Testing — behavioral corpus and ABI parity are exercised; new shapes have coverage; no regressions in conformance/e2e
5. AI slop — redundant comments restating code, verbose filler phrases, excessive blank lines, unnecessary docstrings
6. Architecture — changes live at the right layer (checker/resolve vs ir_lower vs jvm emit); no shape-gated hacks or workarounds where a generic solution belongs
