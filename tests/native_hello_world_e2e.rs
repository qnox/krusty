//! Kotlin to a native executable, end to end.
//!
//! This is the test the native track exists to pass: source in, a binary out, the binary RUN and
//! its output compared. Nothing here inspects generated C — a C program that looks right and
//! prints the wrong thing is exactly the failure a code-shape assertion cannot catch, and the
//! whole point of `docs/BUILD_AND_NATIVE_PLAN.md`'s native phases is to produce programs that run.
//!
//! The emitted program contains no JVM. Its symbols come from the Kotlin/JVM stdlib jar only
//! because that is krusty's one symbol provider (phase 7 replaces it with klib ingestion); the
//! executable links against the generated `krusty_rt.c` and the C library, and nothing else.
//!
//! Skips rather than fails when the Kotlin stdlib or a C compiler is unavailable.

use std::path::{Path, PathBuf};

use krusty::backend::Artifact;
use krusty::diag::DiagSink;
use krusty::jvm::classpath::Classpath;
use krusty::native::{NativeBackend, NativeTarget};
use krusty::source::SourceInput;

/// A scratch directory that cleans itself up.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "krusty-native-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos())
        ));
        let _ = std::fs::remove_dir_all(&path);
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Compile `sources` with the native backend, returning its artifacts and diagnostics.
fn compile(sources: &[(&str, &str)]) -> (Vec<Artifact>, Vec<String>) {
    let jar = krusty::toolchain::stdlib_jar().expect("checked by the caller");
    let classpath = std::rc::Rc::new(Classpath::new(vec![jar]));
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(
        classpath.clone(),
    ));

    let inputs = sources
        .iter()
        .map(|(stem, source)| SourceInput::kotlin(source).with_file_stem(stem))
        .collect::<Vec<_>>();
    let stems = sources
        .iter()
        .map(|(stem, _)| (*stem).to_string())
        .collect::<Vec<_>>();
    let mut features = krusty::features::LangFeatures::new();
    for (_, source) in sources {
        features.apply_source_directives(source);
    }

    let mut diags = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs, platform, &features, &mut diags,
    );
    let backend = NativeBackend::new(classpath);
    let artifacts = krusty::compiler::emit_analyzed(analysis, &stems, &backend, "main", &mut diags);
    let diagnostics = diags
        .diags
        .into_iter()
        .map(|diagnostic| diagnostic.msg)
        .collect();
    (artifacts, diagnostics)
}

/// Whether this environment can run the native tests at all.
fn available() -> bool {
    krusty::toolchain::stdlib_jar().is_some()
        && NativeTarget::host().is_some_and(krusty::native::can_build)
}

/// Compile, link and run a single-file program; return its standard output.
fn run(source: &str) -> String {
    let (artifacts, diagnostics) = compile(&[("Main", source)]);
    assert!(
        diagnostics.is_empty(),
        "the native backend rejected the program: {diagnostics:?}"
    );

    let scratch = Scratch::new("run");
    let executable = scratch.path().join("program");
    krusty::native::link_executable(
        &artifacts,
        scratch.path(),
        &executable,
        NativeTarget::host().expect("checked by `available`"),
    )
    .unwrap_or_else(|error| panic!("the generated C must compile: {error}"));

    let output = std::process::Command::new(&executable)
        .output()
        .expect("run the built executable");
    assert!(
        output.status.success(),
        "the built executable must exit cleanly: {}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn a_kotlin_hello_world_builds_and_runs_natively() {
    if !available() {
        eprintln!("skipping: needs the Kotlin stdlib and a C compiler");
        return;
    }
    assert_eq!(
        run("fun main() {\n    println(\"Hello, world!\")\n}\n"),
        "Hello, world!\n"
    );
}

#[test]
fn the_built_executable_is_native_and_does_not_need_a_jvm() {
    if !available() {
        eprintln!("skipping: needs the Kotlin stdlib and a C compiler");
        return;
    }
    let (artifacts, diagnostics) = compile(&[("Main", "fun main() { println(\"hi\") }")]);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert!(
        artifacts.iter().all(|(name, _)| !name.ends_with(".class")),
        "a native build must not emit class files: {:?}",
        artifacts.iter().map(|(name, _)| name).collect::<Vec<_>>()
    );

    let scratch = Scratch::new("native");
    let executable = scratch.path().join("program");
    krusty::native::link_executable(
        &artifacts,
        scratch.path(),
        &executable,
        NativeTarget::host().expect("checked by `available`"),
    )
    .expect("link");

    // Run it with `JAVA_HOME` and `PATH` emptied. A program that still prints cannot have reached
    // a JVM; this is the claim "native" is making, so it is worth asserting rather than assuming.
    let output = std::process::Command::new(&executable)
        .env_clear()
        .output()
        .expect("run");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "hi\n");
}

#[test]
fn arithmetic_locals_and_control_flow_run() {
    if !available() {
        eprintln!("skipping: needs the Kotlin stdlib and a C compiler");
        return;
    }
    assert_eq!(run("fun main() { println(1 + 2) }"), "3\n");
    assert_eq!(
        run("fun main() {\n\
             \x20   var i = 0\n\
             \x20   while (i < 3) { println(i); i = i + 1 }\n\
             }\n"),
        "0\n1\n2\n",
        "a `while` with a mutable local"
    );
    assert_eq!(
        run("fun main() { for (i in 1..3) println(i) }"),
        "1\n2\n3\n",
        "a lowered `for` puts its step in a statement update and its overflow guard in a \
         labeled `break`; C can express neither directly"
    );
}

#[test]
fn a_function_call_and_recursion_run() {
    if !available() {
        eprintln!("skipping: needs the Kotlin stdlib and a C compiler");
        return;
    }
    assert_eq!(
        run(
            "fun greet(name: String): String = \"Hello, \" + name + \"!\"\n\
             fun main() { println(greet(\"world\")) }\n"
        ),
        "Hello, world!\n"
    );
    assert_eq!(
        run(
            "fun fib(n: Int): Int = if (n < 2) n else fib(n - 1) + fib(n - 2)\n\
             fun main() { println(fib(10)) }\n"
        ),
        "55\n",
        "`if` in expression position, and a self-recursive call"
    );
}

#[test]
fn a_string_template_renders_its_values() {
    if !available() {
        eprintln!("skipping: needs the Kotlin stdlib and a C compiler");
        return;
    }
    assert_eq!(
        run("fun main() {\n\
             \x20   val n = 5\n\
             \x20   println(\"n = $n, twice = ${n * 2}\")\n\
             }\n"),
        "n = 5, twice = 10\n"
    );
}

#[test]
fn non_ascii_text_survives_the_round_trip() {
    if !available() {
        eprintln!("skipping: needs the Kotlin stdlib and a C compiler");
        return;
    }
    // The emitter escapes every non-ASCII byte octally. A hex escape would have merged with the
    // following digit; this asserts the bytes come out the other end unchanged.
    assert_eq!(
        run("fun main() { println(\"héllo → 世界 1\") }"),
        "héllo → 世界 1\n"
    );
}

#[test]
fn integer_arithmetic_follows_kotlin_where_c_is_undefined() {
    if !available() {
        eprintln!("skipping: needs the Kotlin stdlib and a C compiler");
        return;
    }
    // Every value below goes through a function so the answer cannot come from constant folding
    // in the Rust compiler, the Kotlin frontend, or `cc`. These are the cases where emitting C's
    // operator directly would be undefined behavior — which at -O0 happens to work on some targets
    // and silently produces a different answer on others.
    let program = "fun add(a: Int, b: Int): Int = a + b\n\
                   fun negate(a: Int): Int = -a\n\
                   fun divide(a: Int, b: Int): Int = a / b\n\
                   fun shiftLeft(a: Int, bits: Int): Int = a shl bits\n\
                   fun shiftRightUnsigned(a: Int, bits: Int): Int = a ushr bits\n\
                   fun main() {\n\
                   \x20   val min = -2147483647 - 1\n\
                   \x20   println(add(2147483647, 1))\n\
                   \x20   println(negate(min))\n\
                   \x20   println(divide(min, -1))\n\
                   \x20   println(shiftLeft(1, 32))\n\
                   \x20   println(shiftRightUnsigned(-1, 28))\n\
                   }\n";
    assert_eq!(
        run(program),
        "-2147483648\n-2147483648\n-2147483648\n1\n15\n",
        "Kotlin wraps on overflow and masks shift counts; C leaves both undefined"
    );
}

#[test]
fn structural_equality_on_references_is_declined_rather_than_compared_by_address() {
    if !available() {
        eprintln!("skipping: needs the Kotlin stdlib and a C compiler");
        return;
    }
    // `"a" == "b"` is `equals`, not pointer identity. Emitting C's `==` would compile, link, run,
    // and answer the wrong question — the exact failure mode this backend refuses to have.
    let (_, diagnostics) = compile(&[(
        "Main",
        "fun same(a: String, b: String): Boolean = a == b\nfun main() { println(same(\"a\", \"a\")) }\n",
    )]);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("structural equality on references")),
        "expected the backend to decline, got {diagnostics:?}"
    );
}

#[test]
fn printing_a_floating_point_value_is_declined_at_compile_time() {
    if !available() {
        eprintln!("skipping: needs the Kotlin stdlib and a C compiler");
        return;
    }
    // Kotlin's `Double.toString` is the shortest-round-trip algorithm: `1.0` prints as `1.0` and
    // `1e20` as `1.0E20`. Nothing here implements it, so the runtime has no way to render a
    // floating-point value and no box to put one in — and the backend must say so rather than
    // produce a program that prints something Kotlin never would.
    let (_, diagnostics) = compile(&[("Main", "fun main() { println(1.5) }")]);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("native backend does not support")),
        "expected a declining diagnostic, got {diagnostics:?}"
    );

    // Arithmetic and comparison on floating-point values are unaffected — only rendering is.
    assert_eq!(
        run("fun bigger(a: Double, b: Double): Boolean = a > b\n\
             fun main() { println(bigger(2.5, 1.5)) }\n"),
        "true\n"
    );
}

#[test]
fn an_unsupported_construct_is_declined_with_a_diagnostic() {
    if !available() {
        eprintln!("skipping: needs the Kotlin stdlib and a C compiler");
        return;
    }
    // Classes are not implemented yet. The contract is that the backend SAYS so: emitting a
    // partial program that links and misbehaves would be far worse than refusing.
    let (artifacts, diagnostics) = compile(&[(
        "Main",
        "class Point(val x: Int)\nfun main() { println(Point(1).x) }\n",
    )]);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("native backend does not support")),
        "expected a declining diagnostic, got {diagnostics:?}"
    );
    assert!(
        artifacts.iter().all(|(name, _)| name != "Main.c"),
        "a declined file must emit no translation unit"
    );
}

#[test]
fn a_library_module_emits_no_entry_point() {
    if !available() {
        eprintln!("skipping: needs the Kotlin stdlib and a C compiler");
        return;
    }
    let (artifacts, diagnostics) = compile(&[("Greeter", "fun greet(): String = \"hi\"\n")]);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let names = artifacts
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"Greeter.c"));
    assert!(names.contains(&krusty::native::RUNTIME_SOURCE));
    assert!(
        names.contains(&krusty::native::GC_SOURCE),
        "the collector is part of the runtime, not of a program: {names:?}"
    );
    assert!(
        !names.contains(&krusty::native::START_SOURCE),
        "a library has no process entry point: {names:?}"
    );
    assert!(
        !names.contains(&krusty::native::ENTRY_SOURCE),
        "a module with no `main` is a library; inventing an entry point for it would produce a \
         program that silently does nothing: {names:?}"
    );
}

#[test]
fn a_program_that_allocates_heavily_runs_in_bounded_memory() {
    if !available() {
        eprintln!("skipping: needs the Kotlin stdlib and a C compiler");
        return;
    }
    // Each iteration builds a fresh string through a template — several heap objects, all garbage
    // by the next iteration except the one `last` holds. 200,000 iterations is tens of megabytes
    // of allocation, far past the collector's threshold, so this runs through many automatic
    // collections with `last` (and the literals) rooted on the stack the whole time. Before the
    // runtime had a collector this program grew without bound; the check that it no longer does
    // is `tests/native_gc_e2e.rs`, which reads the heap size from C. A bounded-memory assertion
    // belongs here too once the runtime exposes heap statistics to Kotlin.
    assert_eq!(
        run("fun main() {\n\
             \x20   var i = 0\n\
             \x20   var last = \"\"\n\
             \x20   while (i < 200000) {\n\
             \x20       last = \"item-$i:${i * 2}\"\n\
             \x20       i = i + 1\n\
             \x20   }\n\
             \x20   println(last)\n\
             \x20   println(\"done after $i\")\n\
             }\n"),
        "item-199999:399998\ndone after 200000\n"
    );
}

/// The ELF header's `e_machine` field, which says what architecture a binary is for.
fn elf_machine(path: &Path) -> u16 {
    let bytes = std::fs::read(path).expect("read the built executable");
    assert!(bytes.len() > 20, "too short to be an ELF file");
    assert_eq!(&bytes[..4], b"\x7fELF", "not an ELF file");
    u16::from_le_bytes([bytes[18], bytes[19]])
}

#[test]
fn one_host_builds_an_executable_for_every_supported_architecture() {
    if !available() {
        eprintln!("skipping: needs the Kotlin stdlib and a C compiler");
        return;
    }
    // This is the Go property the native track is required to have: every target built from any
    // host, with no per-target toolchain installed. It works because the emitted runtime is
    // freestanding — no target libc, so no sysroot to find — and because one clang compiles every
    // architecture while one `ld.lld` links them.
    let (artifacts, diagnostics) =
        compile(&[("Main", "fun main() { println(\"Hello, world!\") }")]);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");

    let mut built = Vec::new();
    for &target in NativeTarget::ALL {
        if !krusty::native::can_build(target) {
            eprintln!("skipping {target}: this host's C compiler cannot target it");
            continue;
        }
        let scratch = Scratch::new(&format!("cross-{target}"));
        let executable = scratch.path().join("program");
        krusty::native::link_executable(&artifacts, scratch.path(), &executable, target)
            .unwrap_or_else(|error| panic!("building for {target} must succeed: {error}"));
        assert_eq!(
            elf_machine(&executable),
            target.arch.elf_machine(),
            "the binary built for {target} must actually be for {target} — a cross build that \
             silently produced a host binary would pass every other assertion here"
        );
        built.push(target);
    }
    assert!(
        built.len() > 1,
        "cross-compilation is a requirement, not a bonus: only {built:?} could be built"
    );
}

#[test]
fn the_same_source_produces_the_same_binary_for_a_given_target() {
    if !available() {
        eprintln!("skipping: needs the Kotlin stdlib and a C compiler");
        return;
    }
    // Reproducibility is what the build layer's content-addressed cache assumes
    // (`docs/BUILD_AND_NATIVE_PLAN.md`, phase 5). The C the backend emits is the part krusty owns,
    // so that is what this pins; whether `cc` is reproducible is `cc`'s business.
    let first = compile(&[("Main", "fun main() { println(\"Hello, world!\") }")]).0;
    let second = compile(&[("Main", "fun main() { println(\"Hello, world!\") }")]).0;
    assert_eq!(first, second, "emission must be deterministic");
}
