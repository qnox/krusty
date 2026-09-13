//! Kotlin to a native executable through krusty's OWN code generator and linker.
//!
//! No C is emitted anywhere on this path. The frontend produces checked common IR; the Cranelift
//! backend lowers it to a relocatable object; krusty's linker joins that with the runtime that was
//! prebuilt when krusty itself was built, and writes a static ELF executable — which is then RUN
//! and its output compared. Nothing here inspects generated code, for the same reason the rest of
//! the native track never did: code that reads right and prints the wrong thing is exactly what a
//! shape assertion cannot catch.
//!
//! Skips (never fails) when this build of krusty carries no prebuilt runtime for the host, which
//! happens when no C cross-compiler was available at krusty's build time.

use std::path::{Path, PathBuf};

use krusty::backend::Artifact;
use krusty::diag::DiagSink;
use krusty::jvm::classpath::Classpath;
use krusty::native::{CraneliftBackend, NativeTarget};
use krusty::source::SourceInput;

use super::common;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "krusty-codegen-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos())
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create scratch directory");
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

fn host() -> Option<NativeTarget> {
    let target = NativeTarget::host()?;
    (krusty::native::can_link(target) && krusty::toolchain::stdlib_jar().is_some())
        .then_some(target)
}

/// Compile `sources` with the Cranelift backend for `target`.
fn compile(sources: &[(&str, &str)], target: NativeTarget) -> (Vec<Artifact>, Vec<String>) {
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
    let backend = CraneliftBackend::new(classpath, target);
    let artifacts = krusty::compiler::emit_analyzed(analysis, &stems, &backend, "main", &mut diags);
    (artifacts, diags.diags.into_iter().map(|d| d.msg).collect())
}

/// Compile, link with krusty's linker, run, and return stdout.
fn run(source: &str) -> String {
    let target = host().expect("checked by the caller");
    let (artifacts, diagnostics) = compile(&[("Main", source)], target);
    assert!(
        diagnostics.is_empty(),
        "the code generator rejected the program: {diagnostics:?}"
    );
    let objects = artifacts
        .iter()
        .map(|(_, bytes)| bytes.as_slice())
        .collect::<Vec<_>>();
    assert!(!objects.is_empty(), "no object was emitted");
    let image = krusty::native::link_program(&objects, target)
        .unwrap_or_else(|error| panic!("krusty's linker must link the program: {error}"));

    let scratch = Scratch::new("run");
    let executable = scratch.path().join("program");
    std::fs::write(&executable, &image).expect("write executable");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
    }
    // `env_clear`: no PATH, no JAVA_HOME — nothing but the binary itself.
    let output = common::run_freshly_written(std::process::Command::new(&executable).env_clear())
        .expect("run the built executable");
    assert!(
        output.status.success(),
        "the built executable must exit cleanly: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn hello_world_runs_through_krustys_own_code_generator_and_linker() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    assert_eq!(
        run("fun main() {\n    println(\"Hello, world!\")\n}\n"),
        "Hello, world!\n"
    );
}

#[test]
fn the_executable_is_a_static_elf_with_no_interpreter() {
    let Some(target) = host() else {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    };
    let (artifacts, diagnostics) = compile(&[("Main", "fun main() { println(\"hi\") }")], target);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert!(
        artifacts.iter().all(|(name, _)| name.ends_with(".o")),
        "the code generator must emit relocatable objects, never source text: {:?}",
        artifacts.iter().map(|(n, _)| n).collect::<Vec<_>>()
    );
    let objects = artifacts
        .iter()
        .map(|(_, b)| b.as_slice())
        .collect::<Vec<_>>();
    let image = krusty::native::link_program(&objects, target).expect("link");
    assert_eq!(&image[..4], b"\x7fELF");
    assert_eq!(
        image[16], 2,
        "ET_EXEC: a static executable, not a relocatable or a PIE"
    );
    assert_eq!(
        u16::from_le_bytes([image[18], image[19]]),
        target.arch.elf_machine()
    );
    // Two PT_LOAD program headers and no PT_INTERP: nothing loads this but the kernel.
    assert_eq!(
        u16::from_le_bytes([image[56], image[57]]),
        2,
        "program header count"
    );
    for i in 0..2 {
        let off = 64 + 56 * i;
        assert_eq!(
            u32::from_le_bytes(image[off..off + 4].try_into().unwrap()),
            1,
            "PT_LOAD"
        );
    }
}

/// The ELF header's `e_machine`: what architecture a binary is for.
fn elf_machine(image: &[u8]) -> u16 {
    u16::from_le_bytes([image[18], image[19]])
}

#[test]
fn one_host_links_a_static_executable_for_every_supported_architecture() {
    if krusty::toolchain::stdlib_jar().is_none() {
        eprintln!("skipping: needs the Kotlin stdlib");
        return;
    }
    // The Go property, now with nothing but krusty in the loop: the code generator emits each
    // architecture's object and krusty's linker joins it with that architecture's prebuilt
    // runtime. Every produced binary is asserted to be for the architecture asked for; the host's
    // is also RUN. The others cannot be executed here (no emulator), so for them the assertion is
    // that the link resolved every symbol and every relocation — a wrong relocation kind or an
    // out-of-range field fails the link, not the run. The program uses a class hierarchy so the
    // descriptors, vtables and constructor path are cross-compiled too, not only straight code.
    let mut linked = Vec::new();
    for &target in NativeTarget::ALL {
        if !krusty::native::can_link(target) {
            eprintln!("skipping {target}: no prebuilt runtime in this build");
            continue;
        }
        let (artifacts, diagnostics) = compile(
            &[(
                "Main",
                "open class Greeting(val who: String) { open fun text(): String = \"Hello, $who!\" }\n\
                 class Counted(who: String, val n: Long) : Greeting(who) {\n\
                 \x20   override fun text(): String = super.text() + \" $n\"\n\
                 }\n\
                 fun fib(n: Int): Int = if (n < 2) n else fib(n - 1) + fib(n - 2)\n\
                 fun main() {\n\
                 \x20   var total = 0L\n\
                 \x20   for (i in 1..10) { if (i % 2 == 0) continue; total = total + fib(i) }\n\
                 \x20   val g: Greeting = Counted(\"world\", total)\n\
                 \x20   println(g.text())\n\
                 }\n",
            )],
            target,
        );
        assert!(diagnostics.is_empty(), "{target}: {diagnostics:?}");
        let objects = artifacts
            .iter()
            .map(|(_, b)| b.as_slice())
            .collect::<Vec<_>>();
        let image = krusty::native::link_program(&objects, target)
            .unwrap_or_else(|error| panic!("linking for {target} must succeed: {error}"));
        assert_eq!(&image[..4], b"\x7fELF", "{target}");
        assert_eq!(image[16], 2, "{target}: ET_EXEC");
        assert_eq!(
            elf_machine(&image),
            target.arch.elf_machine(),
            "{target}: wrong machine"
        );
        if Some(target) == NativeTarget::host() {
            let scratch = Scratch::new(&format!("cross-{target}"));
            let executable = scratch.path().join("program");
            std::fs::write(&executable, &image).expect("write");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
                    .unwrap();
            }
            let output =
                common::run_freshly_written(std::process::Command::new(&executable).env_clear())
                    .expect("run");
            assert_eq!(
                String::from_utf8_lossy(&output.stdout),
                "Hello, world! 55\n",
                "{target}"
            );
        }
        linked.push(target);
    }
    assert!(
        linked.len() > 1,
        "cross-compilation is a requirement, not a bonus: only {linked:?} could be linked"
    );
}

#[test]
fn arithmetic_locals_and_control_flow_run() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
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
        "a lowered `for`: its step is a statement update and its overflow guard a labeled `break`"
    );
    assert_eq!(
        run("fun main() {\n\
             \x20   var i = 0\n\
             \x20   do { i = i + 1; if (i == 2) continue; println(i) } while (i < 4)\n\
             \x20   outer@ for (a in 1..3) { for (b in 1..3) { if (b == 2) break@outer; println(\"$a$b\") } }\n\
             }\n"),
        "1\n3\n4\n11\n",
        "`do…while`, `continue`, and a labeled `break` out of a nested loop"
    );
}

#[test]
fn a_function_call_and_recursion_run() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
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
    assert_eq!(
        run("fun sign(n: Int): String {\n\
             \x20   if (n < 0) return \"negative\"\n\
             \x20   return when { n == 0 -> \"zero\"; else -> \"positive\" }\n\
             }\n\
             fun main() { println(sign(-3)); println(sign(0)); println(sign(7)) }\n"),
        "negative\nzero\npositive\n",
        "an early `return` inside an `if` leaves the arm with no edge to the merge"
    );
}

#[test]
fn a_string_template_renders_its_values() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    assert_eq!(
        run("fun main() {\n\
             \x20   val n = 5\n\
             \x20   val flag = true\n\
             \x20   println(\"n = $n, twice = ${n * 2}, $flag, ${'c'}, ${10L}\")\n\
             \x20   println(\"$n\")\n\
             }\n"),
        "n = 5, twice = 10, true, c, 10\n5\n"
    );
}

#[test]
fn non_ascii_text_survives_the_round_trip() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    assert_eq!(
        run("fun main() { println(\"héllo → 世界 1\") }"),
        "héllo → 世界 1\n"
    );
}

#[test]
fn integer_arithmetic_follows_kotlin_where_the_machine_traps_or_wraps_differently() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // Every value goes through a function so the answer cannot come from constant folding in the
    // frontend. These are the cases where the machine's own instruction would trap (`idiv` on
    // `MIN_VALUE / -1`), ignore the count, or wrap differently from Kotlin.
    let program = "fun add(a: Int, b: Int): Int = a + b\n\
                   fun negate(a: Int): Int = -a\n\
                   fun divide(a: Int, b: Int): Int = a / b\n\
                   fun remainder(a: Int, b: Int): Int = a % b\n\
                   fun shiftLeft(a: Int, bits: Int): Int = a shl bits\n\
                   fun shiftRight(a: Int, bits: Int): Int = a shr bits\n\
                   fun shiftRightUnsigned(a: Int, bits: Int): Int = a ushr bits\n\
                   fun wide(a: Long, b: Long): Long = a * b\n\
                   fun narrow(a: Byte, b: Byte): Int = a + b\n\
                   fun main() {\n\
                   \x20   val min = -2147483647 - 1\n\
                   \x20   println(add(2147483647, 1))\n\
                   \x20   println(negate(min))\n\
                   \x20   println(divide(min, -1))\n\
                   \x20   println(remainder(-7, 3))\n\
                   \x20   println(shiftLeft(1, 32))\n\
                   \x20   println(shiftRight(-16, 2))\n\
                   \x20   println(shiftRightUnsigned(-1, 28))\n\
                   \x20   println(wide(4000000000L, 3L))\n\
                   \x20   println(narrow(100, 100))\n\
                   }\n";
    assert_eq!(
        run(program),
        "-2147483648\n-2147483648\n-2147483648\n-1\n1\n-4\n15\n12000000000\n200\n",
        "Kotlin wraps on overflow, masks shift counts, and widens `Byte + Byte` to `Int`"
    );
}

#[test]
fn comparisons_and_boolean_logic_run() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    assert_eq!(
        run("fun bigger(a: Double, b: Double): Boolean = a > b\n\
             fun between(x: Int, lo: Int, hi: Int): Boolean = x >= lo && x <= hi\n\
             fun either(a: Boolean, b: Boolean): Boolean = a || b\n\
             fun order(a: Char, b: Char): Int = a.compareTo(b)\n\
             fun main() {\n\
             \x20   println(bigger(2.5, 1.5))\n\
             \x20   println(bigger(1.5, 2.5))\n\
             \x20   println(between(5, 1, 10))\n\
             \x20   println(between(15, 1, 10))\n\
             \x20   println(either(false, true))\n\
             \x20   println(order('a', 'b'))\n\
             \x20   println(1L == 1L)\n\
             \x20   println(2 != 2)\n\
             }\n"),
        "true\nfalse\ntrue\nfalse\ntrue\n-1\ntrue\nfalse\n"
    );
}

#[test]
fn a_null_check_and_identity_stay_pointer_comparisons() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // `x == null` is `x === null` in Kotlin — no `equals` is ever called, so it must not go through
    // the runtime's structural comparison (which would be a call on a null receiver). `===` is
    // address identity on references, and stays one even when the two values are equal.
    assert_eq!(
        run("fun build(n: Int): String = \"value-$n\"\n\
             fun main() {\n\
             \x20   val s: String? = build(1)\n\
             \x20   val n: String? = null\n\
             \x20   println(s == null)\n\
             \x20   println(n == null)\n\
             \x20   println(n != null)\n\
             \x20   println(s === s)\n\
             \x20   println(build(1) === build(1))\n\
             \x20   println(build(1) == build(1))\n\
             }\n"),
        "false\ntrue\nfalse\ntrue\nfalse\ntrue\n",
        "`===` compares addresses where `==` compares contents"
    );
}

#[test]
fn printing_a_floating_point_value_is_declined_at_compile_time() {
    let Some(target) = host() else {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    };
    // Kotlin's `Double.toString` is the shortest-round-trip algorithm. Nothing implements it yet,
    // so the runtime has no way to render a floating-point value and no box to put one in — and
    // the backend says so rather than producing a program that prints something Kotlin never would.
    let (_, diagnostics) = compile(&[("Main", "fun main() { println(1.5) }")], target);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("native backend does not support")),
        "expected a declining diagnostic, got {diagnostics:?}"
    );
}

#[test]
fn an_unsupported_construct_is_declined_with_a_diagnostic() {
    let Some(target) = host() else {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    };
    // A secondary constructor is not implemented. The contract is that the backend SAYS so:
    // emitting a partial object that links and misbehaves would be far worse than refusing.
    let (artifacts, diagnostics) = compile(
        &[(
            "Main",
            "class Point(val x: Int, val y: Int) {\n\
             \x20   constructor(both: Int) : this(both, both)\n\
             }\n\
             fun main() { println(Point(3).y) }\n",
        )],
        target,
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("native backend does not support")),
        "expected a declining diagnostic, got {diagnostics:?}"
    );
    assert!(
        artifacts.is_empty(),
        "a declined file must emit no object: {:?}",
        artifacts.iter().map(|(name, _)| name).collect::<Vec<_>>()
    );
}

#[test]
fn a_library_module_defines_no_entry_point() {
    let Some(target) = host() else {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    };
    let (artifacts, diagnostics) =
        compile(&[("Greeter", "fun greet(): String = \"hi\"\n")], target);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(
        artifacts
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        vec!["Greeter.o"]
    );
    // A module with no `main` is a library; inventing an entry point for it would produce a program
    // that silently does nothing. Linked alone, the runtime's `_start` has nothing to call.
    let objects = artifacts
        .iter()
        .map(|(_, b)| b.as_slice())
        .collect::<Vec<_>>();
    match krusty::native::link_program(&objects, target) {
        Err(krusty::native::ProgramLinkError::UndefinedSymbol(symbol)) => {
            assert_eq!(symbol, "kt_program_entry");
        }
        other => panic!("a library must not link into a program: {other:?}"),
    }
}

#[test]
fn a_program_that_allocates_heavily_runs_under_collection() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // Each iteration builds a fresh string through a template — several heap objects, all garbage
    // by the next iteration except the one `last` holds. 200,000 iterations is tens of megabytes
    // of allocation, far past the collector's threshold, so this runs through many automatic
    // collections with `last` and the literals rooted in registers and stack slots the whole time
    // — the conservative root scan has to find them where Cranelift put them.
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

#[test]
fn the_same_source_produces_the_same_object_for_a_given_target() {
    let Some(target) = host() else {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    };
    // Reproducibility is what the build layer's content-addressed cache assumes
    // (`docs/BUILD_AND_NATIVE_PLAN.md`, phase 5), and with no other compiler in the loop the whole
    // object is krusty's to pin — not only the source it used to emit.
    let program = "fun fib(n: Int): Int = if (n < 2) n else fib(n - 1) + fib(n - 2)\n\
                   fun main() { for (i in 1..5) println(\"$i: ${fib(i)}\") }\n";
    let first = compile(&[("Main", program)], target).0;
    let second = compile(&[("Main", program)], target).0;
    assert!(!first.is_empty());
    assert_eq!(first, second, "code generation must be deterministic");
}

#[test]
fn a_string_literal_with_an_unpaired_surrogate_is_declined() {
    let Some(target) = host() else {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    };
    // Kotlin admits a lone surrogate in a string; UTF-8 cannot encode one and the runtime's strings
    // are UTF-8. Emitting some other bytes would be a silent miscompilation, so the file declines.
    let (artifacts, diagnostics) =
        compile(&[("Main", "fun main() { println(\"\\uD800\") }")], target);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("unpaired surrogate")),
        "expected the backend to decline, got {diagnostics:?}"
    );
    assert!(artifacts.is_empty());
}

#[test]
fn identity_equality_on_primitives_compares_values() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // `===` between two `Long`s is value equality in Kotlin (identity on primitives is `==`, with
    // a deprecation warning). Boxing each side and comparing the boxes would answer `false`.
    assert_eq!(
        run("fun id(x: Long) = x\n\
             fun add(a: Long, b: Long) = a + b\n\
             fun main() {\n\
             \x20   println(id(0L) === id(0L))\n\
             \x20   println(id(add(123456789L, 1L)) !== id(123456790L))\n\
             \x20   val s = \"x\"\n\
             \x20   println(s === s)\n\
             }\n"),
        "true\nfalse\ntrue\n"
    );
}

#[test]
fn unsigned_integers_are_declined_rather_than_carried_as_signed() {
    let Some(target) = host() else {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    };
    // `UInt` is a value class over `Int`. Carrying it as the `Int` it wraps would make
    // `1u as? Int` succeed and `UInt.MAX_VALUE` print as `-1`; until the generator models the
    // wrapper, the file declines.
    let (_, diagnostics) = compile(
        &[(
            "Main",
            "fun same(x: UInt) = x as? Int\nfun main() { println(same(1u) == null) }\n",
        )],
        target,
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("unsigned integer type")),
        "expected the backend to decline, got {diagnostics:?}"
    );
}

#[test]
fn top_level_properties_initialize_before_main_and_hold_their_values() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // Initializers run in declaration order, before the entry function — where the JVM would have
    // run the facade's `<clinit>`. `tag` prints as each one is evaluated, so a reordering shows up
    // in the output, and `derived` reads `base`, which only works if `base` was assigned first.
    assert_eq!(
        run("fun tag(s: String, n: Int): Int { println(s); return n }\n\
             val base: Int = tag(\"base\", 4)\n\
             val derived: Int = base * 10\n\
             var counter: Int = 0\n\
             val name: String = \"kotlin\"\n\
             fun bump(): Int { counter = counter + 1; return counter }\n\
             fun main() {\n\
             \x20   println(base)\n\
             \x20   println(derived)\n\
             \x20   println(name)\n\
             \x20   println(bump())\n\
             \x20   println(bump())\n\
             \x20   counter = 40\n\
             \x20   println(bump())\n\
             }\n"),
        "base\n4\n40\nkotlin\n1\n2\n41\n"
    );
}

#[test]
fn a_top_level_property_is_a_collector_root() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // `kept` holds a HEAP string (built by a template, so it is not a literal in static storage)
    // and nothing else references it: only the registered global root keeps it alive. The loop
    // then allocates far past the collection threshold, so the string survives many collections
    // with its slot as its sole root. Without `kt_gc_add_global_root` this printed reused bytes.
    assert_eq!(
        run("val kept: String = \"kept-${1 + 1}\"\n\
             var last: String = \"\"\n\
             fun main() {\n\
             \x20   var i = 0\n\
             \x20   while (i < 100000) {\n\
             \x20       last = \"garbage-$i\"\n\
             \x20       i = i + 1\n\
             \x20   }\n\
             \x20   println(kept)\n\
             \x20   println(last)\n\
             }\n"),
        "kept-2\ngarbage-99999\n"
    );
}

#[test]
fn a_top_level_property_with_custom_accessors_runs_their_bodies() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // `doubled` has no storage at all — a read is a call. `guarded` has storage and both accessors
    // written in source; `field` inside them is the backing slot, not a recursive accessor call.
    assert_eq!(
        run("var backing: Int = 3\n\
             val doubled: Int get() = backing * 2\n\
             var guarded: Int = 1\n\
             \x20   get() = field + 100\n\
             \x20   set(v) { field = if (v < 0) 0 else v }\n\
             fun main() {\n\
             \x20   println(doubled)\n\
             \x20   backing = 5\n\
             \x20   println(doubled)\n\
             \x20   println(guarded)\n\
             \x20   guarded = -7\n\
             \x20   println(guarded)\n\
             \x20   guarded = 7\n\
             \x20   println(guarded)\n\
             }\n"),
        "6\n10\n101\n100\n107\n"
    );
}

#[test]
fn structural_equality_on_references_asks_the_receiver() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // `==` on references is `equals`, not an address comparison: two separately built strings with
    // the same text are equal, a class with an overridden `equals` answers for itself, one without
    // falls back to identity, and `null` equals only `null`. Every value here is built at runtime
    // so no literal can be shared into a false positive.
    assert_eq!(
        run("class Point(val x: Int, val y: Int) {\n\
             \x20   override fun equals(other: Any?): Boolean = other is Point && other.x == x && other.y == y\n\
             \x20   override fun hashCode(): Int = x * 31 + y\n\
             }\n\
             class Opaque(val v: Int)\n\
             fun build(n: Int): String = \"value-$n\"\n\
             fun main() {\n\
             \x20   println(build(1) == build(1))\n\
             \x20   println(build(1) == build(2))\n\
             \x20   println(Point(1, 2) == Point(1, 2))\n\
             \x20   println(Point(1, 2) == Point(3, 4))\n\
             \x20   println(Point(1, 2) != Point(3, 4))\n\
             \x20   println(Opaque(1) == Opaque(1))\n\
             \x20   val o = Opaque(1)\n\
             \x20   println(o == o)\n\
             \x20   val missing: String? = null\n\
             \x20   println(missing == build(1))\n\
             \x20   println(build(1) == missing)\n\
             \x20   val boxed: Int? = 5\n\
             \x20   println(boxed == 5)\n\
             \x20   println(boxed == 6)\n\
             }\n"),
        "true\nfalse\ntrue\nfalse\ntrue\nfalse\ntrue\nfalse\nfalse\ntrue\nfalse\n"
    );
}

#[test]
fn a_when_whose_arms_have_different_types_is_carried_as_a_reference() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // `when (s) { "a" -> 1; else -> null }` is `Int?`. Taking the first arm's type would carry it
    // as `Int` and store the `null` arm's pointer into a 32-bit slot — so the result is a
    // reference whenever the arms' carriers disagree, and the `Int` arm boxes on its way out.
    assert_eq!(
        run("fun pick(s: String): Int? = when (s) {\n\
             \x20   \"a\" -> 1\n\
             \x20   \"b\" -> 2\n\
             \x20   else -> null\n\
             }\n\
             fun describe(n: Int): Any = if (n > 0) n else \"negative\"\n\
             fun main() {\n\
             \x20   println(pick(\"a\"))\n\
             \x20   println(pick(\"b\"))\n\
             \x20   println(pick(\"z\"))\n\
             \x20   println(pick(\"z\") == null)\n\
             \x20   println(pick(\"a\") == 1)\n\
             \x20   println(describe(5))\n\
             \x20   println(describe(-5))\n\
             }\n"),
        "1\n2\nnull\ntrue\ntrue\n5\nnegative\n"
    );
}

#[test]
fn boxing_a_small_value_hands_out_the_same_object() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // Kotlin lets a program observe box identity for small values (`===` on two boxings of `true`
    // is true), because both the JVM and Kotlin/Native cache them. The cached range is the JVM's:
    // -128..127 for the integer types and both `Boolean`s. Above it, identity is unspecified and
    // only equality is asserted here.
    assert_eq!(
        run("fun boxed(b: Boolean): Any = b\n\
             fun boxedInt(n: Int): Any = n\n\
             fun main() {\n\
             \x20   println(boxed(true) === boxed(true))\n\
             \x20   println(boxed(false) === boxed(false))\n\
             \x20   println(boxed(true) === boxed(false))\n\
             \x20   println(boxed(true) == boxed(true))\n\
             \x20   println(boxedInt(127) === boxedInt(127))\n\
             \x20   println(boxedInt(-128) === boxedInt(-128))\n\
             \x20   println(boxedInt(5) === boxedInt(6))\n\
             \x20   println(boxedInt(1000) == boxedInt(1000))\n\
             \x20   println(boxedInt(42))\n\
             \x20   println(boxedInt(1000))\n\
             }\n"),
        "true\ntrue\nfalse\ntrue\ntrue\ntrue\nfalse\ntrue\n42\n1000\n"
    );
}

#[test]
fn a_not_null_assertion_passes_a_value_through_and_fails_on_null() {
    let Some(target) = host() else {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    };
    assert_eq!(
        run(
            "fun maybe(n: Int): String? = if (n > 0) \"value-$n\" else null\n\
             fun boxed(n: Int): Int? = if (n > 0) n else null\n\
             fun main() {\n\
             \x20   println(maybe(1)!!)\n\
             \x20   println(boxed(7)!! + 1)\n\
             }\n"
        ),
        "value-1\n8\n"
    );
    // On null it must fail loudly rather than carry the null onward: there are no exceptions yet,
    // so the honest realization is a diagnosable exit naming what happened.
    let (artifacts, diagnostics) = compile(
        &[(
            "Main",
            "fun maybe(n: Int): String? = if (n > 0) \"value-$n\" else null\n\
             fun main() { println(\"before\"); println(maybe(0)!!) }\n",
        )],
        target,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let objects = artifacts
        .iter()
        .map(|(_, bytes)| bytes.as_slice())
        .collect::<Vec<_>>();
    let image = krusty::native::link_program(&objects, target).expect("link");
    let scratch = Scratch::new("notnull");
    let executable = scratch.path().join("program");
    std::fs::write(&executable, &image).expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let output = common::run_freshly_written(std::process::Command::new(&executable).env_clear())
        .expect("run");
    assert!(!output.status.success(), "a `!!` on null must not continue");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "before\n");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("null cannot be cast to a non-null type"),
        "stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_lambda_is_an_object_that_can_be_passed_called_and_returned() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // A function value crosses a call boundary, is stored in a local, and is invoked through a
    // parameter that knows only its arity — so every function value of an arity has to be callable
    // one way, which is what the boxing convention at the thunk is for.
    assert_eq!(
        run("fun applyTo(f: (Int) -> Int, n: Int): Int = f(n)\n\
             fun twice(f: (Int) -> Int, n: Int): Int = f(f(n))\n\
             fun adder(by: Int): (Int) -> Int = { x -> x + by }\n\
             fun main() {\n\
             \x20   val inc = { x: Int -> x + 1 }\n\
             \x20   println(applyTo(inc, 5))\n\
             \x20   println(twice(inc, 5))\n\
             \x20   println(applyTo(adder(10), 5))\n\
             \x20   println(applyTo({ x -> x * x }, 7))\n\
             \x20   val join = { a: String, b: String -> a + b }\n\
             \x20   println(join(\"na\", \"me\"))\n\
             \x20   val nothing = { }\n\
             \x20   nothing()\n\
             \x20   println(\"done\")\n\
             }\n"),
        "6\n7\n15\n49\nname\ndone\n"
    );
}

#[test]
fn a_lambda_captures_a_mutable_local_by_reference() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // A captured `var` is one cell shared by the closure and the frame that made it, not a copy:
    // writes on either side are visible to the other. A captured `val` is just its value.
    assert_eq!(
        run("fun run(f: () -> Unit) { f() }\n\
             fun main() {\n\
             \x20   var count = 0\n\
             \x20   val label = \"n\"\n\
             \x20   val bump = { count = count + 1 }\n\
             \x20   run(bump)\n\
             \x20   run(bump)\n\
             \x20   println(count)\n\
             \x20   count = 10\n\
             \x20   run(bump)\n\
             \x20   println(count)\n\
             \x20   var text = \"\"\n\
             \x20   val append = { s: String -> text = text + label + s }\n\
             \x20   append(\"a\")\n\
             \x20   append(\"b\")\n\
             \x20   println(text)\n\
             }\n"),
        "2\n11\nnanb\n"
    );
}

#[test]
fn a_function_value_survives_collection_with_its_captures() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // The closure holds the only reference to a heap string, and the loop allocates far past the
    // collection threshold while calling it — so the capture has to be traced through the function
    // object's own descriptor, at the offset the generator laid it out at.
    assert_eq!(
        run("fun main() {\n\
             \x20   val kept = \"kept-${1 + 1}\"\n\
             \x20   val describe = { n: Int -> \"$kept:$n\" }\n\
             \x20   var last = \"\"\n\
             \x20   var i = 0\n\
             \x20   while (i < 100000) {\n\
             \x20       last = describe(i)\n\
             \x20       i = i + 1\n\
             \x20   }\n\
             \x20   println(last)\n\
             \x20   println(describe(7))\n\
             }\n"),
        "kept-2:99999\nkept-2:7\n"
    );
}

#[test]
fn a_lambda_that_captures_nothing_is_one_object() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // `{}` written once is one object however often it is evaluated — with nothing to capture
    // there is nothing to allocate, so the instance is static storage and `===` sees it. A lambda
    // that DOES capture is a fresh object each time, because each holds its own captured values.
    // `hashCode` is not asserted here: `equals`/`hashCode` on a function value are declined for
    // now, because a value of function type no longer says whether a lambda or a callable
    // reference produced it, and the two want different answers.
    assert_eq!(
        run("fun generate(): () -> Unit = {}\n\
             fun adder(by: Int): (Int) -> Int = { x -> x + by }\n\
             fun main() {\n\
             \x20   println(generate() === generate())\n\
             \x20   println(adder(1) === adder(1))\n\
             \x20   println(adder(1)(10))\n\
             }\n"),
        "true\nfalse\n11\n"
    );
}

#[test]
fn the_unit_value_is_the_runtimes_own() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // `Unit` is one value for the whole program, so it is the runtime's rather than something each
    // file declares — which is also why a file that merely mentions it needs nothing emitted.
    assert_eq!(
        run("fun nothing(): Unit = Unit\n\
             fun main() {\n\
             \x20   println(nothing())\n\
             \x20   println(nothing() === Unit)\n\
             \x20   val u: Any = Unit\n\
             \x20   println(u == Unit)\n\
             }\n"),
        "kotlin.Unit\ntrue\ntrue\n"
    );
}

#[test]
fn arrays_read_write_and_know_their_size() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // A primitive array and a reference array, built three ways: element by element, sized and
    // zero-filled, and filled by a loop. Every element kind has a stride of its own, and reading
    // back what was written is what proves the generator and the runtime agree on it.
    assert_eq!(
        run("fun main() {\n\
             \x20   val ints = intArrayOf(3, 1, 4)\n\
             \x20   println(ints.size)\n\
             \x20   println(ints[0])\n\
             \x20   ints[0] = ints[2]\n\
             \x20   println(ints[0])\n\
             \x20   val zeros = IntArray(4)\n\
             \x20   println(zeros.size)\n\
             \x20   println(zeros[3])\n\
             \x20   var i = 0\n\
             \x20   var total = 0\n\
             \x20   while (i < ints.size) { total = total + ints[i]; i = i + 1 }\n\
             \x20   println(total)\n\
             \x20   val words = arrayOf(\"a\", \"bb\")\n\
             \x20   println(words.size)\n\
             \x20   println(words[1])\n\
             \x20   words[0] = words[1]\n\
             \x20   println(words[0])\n\
             \x20   val longs = longArrayOf(1L, 2L)\n\
             \x20   println(longs[1])\n\
             \x20   val flags = booleanArrayOf(true, false)\n\
             \x20   println(flags[0])\n\
             \x20   println(flags[1])\n\
             \x20   val chars = charArrayOf('k', 't')\n\
             \x20   println(chars[1])\n\
             \x20   val bytes = byteArrayOf(7, 8)\n\
             \x20   println(bytes[1])\n\
             }\n"),
        "3\n3\n4\n4\n0\n9\n2\nbb\nbb\n2\ntrue\nfalse\nt\n8\n"
    );
}

#[test]
fn an_array_index_outside_its_bounds_fails_loudly() {
    let Some(target) = host() else {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    };
    // Kotlin throws IndexOutOfBoundsException; there are no exceptions yet, so the honest
    // realization is a diagnosable exit. A negative index must be caught by the same check.
    for index in ["3", "-1"] {
        let (artifacts, diagnostics) = compile(
            &[(
                "Main",
                &format!(
                    "fun at(a: IntArray, i: Int): Int = a[i]\n\
                     fun main() {{ println(\"before\"); println(at(intArrayOf(1, 2, 3), {index})) }}\n"
                ),
            )],
            target,
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let objects = artifacts
            .iter()
            .map(|(_, bytes)| bytes.as_slice())
            .collect::<Vec<_>>();
        let image = krusty::native::link_program(&objects, target).expect("link");
        let scratch = Scratch::new("bounds");
        let executable = scratch.path().join("program");
        std::fs::write(&executable, &image).expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let output =
            common::run_freshly_written(std::process::Command::new(&executable).env_clear())
                .expect("run");
        assert!(!output.status.success(), "index {index} must not continue");
        assert_eq!(String::from_utf8_lossy(&output.stdout), "before\n");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("array index out of bounds"),
            "index {index}: {:?}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn a_reference_array_is_traced_through_collection() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // The array holds the only reference to each of its heap strings, and the loop allocates far
    // past the collection threshold — so the elements have to be traced as references, at the
    // stride the descriptor declares. A `LongArray` of the same element width must NOT be walked
    // as pointers, which is why the descriptor carries both the stride and whether to look inside.
    assert_eq!(
        run("fun main() {\n\
             \x20   val kept = arrayOfNulls<String>(3)\n\
             \x20   var i = 0\n\
             \x20   while (i < 3) { kept[i] = \"kept-$i\"; i = i + 1 }\n\
             \x20   val decoys = LongArray(3)\n\
             \x20   i = 0\n\
             \x20   while (i < 3) { decoys[i] = 140737488355328L + i; i = i + 1 }\n\
             \x20   var garbage = \"\"\n\
             \x20   var n = 0\n\
             \x20   while (n < 100000) { garbage = \"garbage-$n\"; n = n + 1 }\n\
             \x20   println(kept[0])\n\
             \x20   println(kept[2])\n\
             \x20   println(decoys[1])\n\
             \x20   println(garbage)\n\
             }\n"),
        "kept-0\nkept-2\n140737488355329\ngarbage-99999\n"
    );
}

#[test]
fn a_reference_array_of_a_primitive_boxes_at_the_element_boundary() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // `Array<Int>` stores BOXED elements, unlike `IntArray`. A read therefore produces a reference
    // where the reader may want an `Int` — the conversion belongs at the element boundary, which
    // is the same place the JVM puts its `checkcast` and `intValue()`.
    assert_eq!(
        run("fun main() {\n\
             \x20   val a = arrayOfNulls<Int>(5)\n\
             \x20   for (i in 0..4) a[i] = i + 1\n\
             \x20   var sum = 0\n\
             \x20   for (el in (a as Array<Int>)) sum = sum + el\n\
             \x20   println(sum)\n\
             \x20   val ints = IntArray(5)\n\
             \x20   for (i in 0..4) ints[i] = i + 1\n\
             \x20   var plain = 0\n\
             \x20   for (el in ints) plain = plain + el\n\
             \x20   println(plain)\n\
             }\n"),
        "15\n15\n"
    );
}

#[test]
fn a_callable_reference_is_a_function_value_like_any_other() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // `::f` and `obj::m` differ from a lambda in how they are written and in nothing else that
    // matters: common lowering synthesizes the adapter that calls the referenced function, and a
    // bound receiver is simply the first value the reference carries. So a reference with nothing
    // bound is one object, as `{}` is.
    assert_eq!(
        run("class Greeter(val name: String) {\n\
             \x20   fun greet(): String = \"hi ${name}\"\n\
             \x20   fun loud(n: Int): String = \"HI ${name} $n\"\n\
             }\n\
             fun double(n: Int): Int = n * 2\n\
             fun applyTo(f: (Int) -> Int, n: Int): Int = f(n)\n\
             fun call(f: () -> String): String = f()\n\
             fun callWith(f: (Int) -> String, n: Int): String = f(n)\n\
             fun main() {\n\
             \x20   println(applyTo(::double, 5))\n\
             \x20   val g = Greeter(\"k\")\n\
             \x20   println(call(g::greet))\n\
             \x20   println(callWith(g::loud, 7))\n\
             \x20   val f = ::double\n\
             \x20   println(f(21))\n\
             \x20   var total = 0\n\
             \x20   val fns = arrayOfNulls<(Int) -> Int>(3)\n\
             \x20   val ref: (Int) -> Int = ::double\n\
             \x20   var i = 0\n\
             \x20   while (i < 3) { fns[i] = ref; i = i + 1 }\n\
             \x20   i = 0\n\
             \x20   while (i < 3) { total = total + fns[i]!!(i); i = i + 1 }\n\
             \x20   println(total)\n\
             }\n"),
        "10\nhi k\nHI k 7\n42\n6\n",
        "a reference is stored, passed and called like any other function value"
    );
}

#[test]
fn comparing_two_function_values_is_declined() {
    let Some(target) = host() else {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    };
    // Kotlin compares function values by the DECLARATION they name and the receiver they bind, so
    // `::f == ::f` is true even though each `::f` is its own object. A function value here has
    // `kotlin.Any`'s identity equality, which would answer `false` — so the comparison is refused
    // rather than answered wrongly. What it needs is one emitted type per referenced declaration,
    // carrying an `equals` that compares the type and the bound receiver.
    let (_, diagnostics) = compile(
        &[(
            "Main",
            "fun double(n: Int): Int = n * 2\n\
             fun main() {\n\
             \x20   val a: (Int) -> Int = ::double\n\
             \x20   val b: (Int) -> Int = ::double\n\
             \x20   println(a == b)\n\
             }\n",
        )],
        target,
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("equality on a function value")),
        "expected the backend to decline, got {diagnostics:?}"
    );
}

#[test]
fn a_unit_tail_call_before_a_bare_return_becomes_a_loop() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // In a `Unit` function a call with nothing after it but `return` IS in tail position, so
    // `tailrec` promises it costs no stack. A million frames is far past what any stack holds, so
    // this program printing its answer is the whole assertion: were the call left recursive the
    // process would die on its stack guard page instead. The non-tail call is the control — it
    // stays an ordinary call, and recurses only the one level the source asks for.
    assert_eq!(
        run("var reached = 0\n\
             tailrec fun countDown(n: Int) {\n\
             \x20   if (n == 0) return\n\
             \x20   if (n == 500000) reached = n\n\
             \x20   countDown(n - 1)\n\
             \x20   return\n\
             }\n\
             tailrec fun branchy(n: Int) {\n\
             \x20   if (n > 500000) {\n\
             \x20       branchy(n - 1)\n\
             \x20   } else if (n > 0) {\n\
             \x20       branchy(n - 1)\n\
             \x20       return\n\
             \x20   }\n\
             }\n\
             fun main() {\n\
             \x20   countDown(1000000)\n\
             \x20   println(reached)\n\
             \x20   branchy(1000000)\n\
             \x20   println(\"deep\")\n\
             }\n"),
        "500000\ndeep\n"
    );
}

#[test]
fn a_tailrec_the_checked_lowering_leaves_recursive_is_declined() {
    let Some(target) = host() else {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    };
    // `tailrec` is a promise about STACK, and only a top-level non-extension function has its tail
    // calls rewritten into a loop today. A member, an extension or a local `tailrec` still recurses,
    // and the source wrote the modifier because it recurses past any stack — so accepting one means
    // emitting a program that dies on a guard page at a depth the source expects to survive. How
    // deep a native stack goes is the machine's business, and a gate must not depend on it, so the
    // generator declines these instead of compiling them into a crash.
    for (label, source) in [
        (
            "a member",
            "class Counter {\n\
             \x20   tailrec fun down(n: Int) { if (n > 0) down(n - 1) }\n\
             }\n\
             fun main() { Counter().down(1000000) }\n",
        ),
        (
            "an extension",
            "tailrec fun Int.down(n: Int) { if (n > 0) this.down(n - 1) }\n\
             fun main() { 1.down(1000000) }\n",
        ),
        (
            "a local",
            "fun main() {\n\
             \x20   tailrec fun down(n: Int) { if (n > 0) down(n - 1) }\n\
             \x20   down(1000000)\n\
             }\n",
        ),
    ] {
        let (_, diagnostics) = compile(&[("Main", source)], target);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.contains("common lowering leaves recursive")),
            "expected {label} `tailrec` to be declined, got {diagnostics:?}"
        );
    }
}

#[test]
fn the_stdlib_scope_functions_are_expanded_at_the_call_site() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // `apply`/`also` yield the receiver, `let`/`run` yield the block's result, and all four hand
    // the block the receiver — evaluated exactly once, which the counter pins: `next()` runs once
    // per call however often the block mentions the value it returned.
    assert_eq!(
        run("class Box(var n: Int) {\n\
             \x20   fun twice(): Int = n * 2\n\
             }\n\
             var calls = 0\n\
             fun next(): Box { calls = calls + 1; return Box(3) }\n\
             fun main() {\n\
             \x20   val applied = next().apply { n = n + 1 }\n\
             \x20   println(applied.n)\n\
             \x20   val also = next().also { it.n = it.n + 10 }\n\
             \x20   println(also.n)\n\
             \x20   println(next().let { it.n + it.twice() })\n\
             \x20   println(next().run { n + twice() })\n\
             \x20   val captured = 100\n\
             \x20   println(next().let { it.n + captured })\n\
             \x20   println(calls)\n\
             }\n"),
        "4\n13\n9\n9\n103\n5\n"
    );
}

#[test]
fn a_scope_function_whose_block_is_a_function_value_runs() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // `apply { … }` written at the call site is spliced by the checked lowering and never reaches
    // the generator. This is the other shape: the block arrives as a function-typed PARAMETER, so
    // there is nothing to splice and the call has to be realized — invoke the block on the
    // receiver, then yield what Kotlin's signature says. The counter pins the receiver being
    // evaluated once.
    assert_eq!(
        run("class Box(var n: Int)\n\
             var made = 0\n\
             fun fresh(): Box { made = made + 1; return Box(2) }\n\
             fun build(instructions: Box.() -> Unit): Box = fresh().apply(instructions)\n\
             fun over(block: (Box) -> Unit): Box = fresh().also(block)\n\
             fun read(block: (Box) -> Int): Int = fresh().let(block)\n\
             fun on(block: Box.() -> Int): Int = fresh().run(block)\n\
             fun main() {\n\
             \x20   println(build { n = n + 5 }.n)\n\
             \x20   println(over { it.n = it.n + 7 }.n)\n\
             \x20   println(read { it.n + 30 })\n\
             \x20   println(on { n + 40 })\n\
             \x20   println(made)\n\
             }\n"),
        "7\n9\n32\n42\n4\n"
    );
}

#[test]
fn an_extension_property_is_read_and_written_through_its_accessors() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // An extension property has no storage of its own — there is no object to keep a field in —
    // so every access is a call to its accessor, with the receiver as an argument. A `var` one
    // writes through its setter, and the receiver is what the setter changes.
    assert_eq!(
        run("class Cell(var value: Int)\n\
             val Cell.doubled: Int get() = value * 2\n\
             var Cell.raised: Int\n\
             \x20   get() = value + 1\n\
             \x20   set(next) { value = next - 1 }\n\
             fun main() {\n\
             \x20   val cell = Cell(20)\n\
             \x20   println(cell.doubled)\n\
             \x20   println(cell.raised)\n\
             \x20   cell.raised = 100\n\
             \x20   println(cell.value)\n\
             \x20   println(cell.doubled)\n\
             }\n"),
        "40\n21\n99\n198\n"
    );
}

#[test]
fn a_data_class_gets_kotlins_equality_hashing_and_rendering() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // A data class is only a data class if its synthesized members answer what Kotlin says. The
    // checked lowering writes those members; what the generator supplies is the per-field hash and
    // comparison they are written in terms of, and each is Kotlin's own answer rather than the
    // machine's: a `Long` folds its halves so the high word survives the truncation to `Int`, a
    // `Boolean` is 1231 or 1237, and a `String` field compares by content. A floating-point field
    // is declined for now — the `toString` synthesized beside `equals` would have to render it.
    assert_eq!(
        run("data class Point(val x: Int, val y: Int)\n\
             data class Tagged(val name: String, val big: Long, val flag: Boolean)\n\
             fun main() {\n\
             \x20   println(Point(1, 2) == Point(1, 2))\n\
             \x20   println(Point(1, 2) == Point(1, 3))\n\
             \x20   println(Point(1, 2).hashCode() == Point(1, 2).hashCode())\n\
             \x20   println(Point(1, 2).hashCode() == Point(2, 1).hashCode())\n\
             \x20   println(Point(1, 2).toString())\n\
             \x20   val (a, b) = Point(3, 4)\n\
             \x20   println(a + b)\n\
             \x20   println(Tagged(\"a\" + \"b\", 1L shl 40, true) == Tagged(\"ab\", 1L shl 40, true))\n\
             \x20   println(Tagged(\"ab\", 1L, true) == Tagged(\"ab\", 2L, true))\n\
             \x20   println(Tagged(\"ab\", 1L, true).hashCode() == Tagged(\"ab\", 1L, true).hashCode())\n\
             }\n"),
        "true\nfalse\ntrue\nfalse\nPoint(x=1, y=2)\n7\ntrue\nfalse\ntrue\n"
    );
}

#[test]
fn an_interface_dispatches_through_a_program_wide_slot() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // A call through an interface-typed value knows only the interface, so the slot number it uses
    // has to mean the same member in every class implementing it — which is what the program-wide
    // numbering above every class's own slots buys. The cases that have to work: a plain override,
    // a default body the class does not override, an interface property, a second interface on the
    // same class, an interface extending another, and a method the class INHERITS rather than
    // declares (`Sub` satisfies `Named` with `Base`'s method, and `Base` knows nothing of `Named`).
    assert_eq!(
        run("interface Named {\n\
             \x20   fun name(): String\n\
             \x20   fun greet(): String = \"hi \" + name()\n\
             \x20   val tag: String\n\
             }\n\
             interface Counted { fun count(): Int }\n\
             interface Both : Named, Counted\n\
             class One : Named {\n\
             \x20   override fun name() = \"one\"\n\
             \x20   override val tag = \"t1\"\n\
             }\n\
             class Two : Both {\n\
             \x20   override fun name() = \"two\"\n\
             \x20   override fun greet() = \"hey \" + name()\n\
             \x20   override fun count() = 2\n\
             \x20   override val tag = \"t2\"\n\
             }\n\
             open class Base { open fun name() = \"base\" }\n\
             class Sub : Base(), Named { override val tag = \"t3\" }\n\
             fun describe(named: Named): String = named.greet() + \"/\" + named.tag\n\
             fun main() {\n\
             \x20   println(describe(One()))\n\
             \x20   println(describe(Two()))\n\
             \x20   println(describe(Sub()))\n\
             \x20   val both: Both = Two()\n\
             \x20   println(both.count())\n\
             \x20   val counted: Counted = Two()\n\
             \x20   println(counted.count())\n\
             }\n"),
        "hi one/t1\nhey two/t2\nhi base/t3\n2\n2\n"
    );
}

#[test]
fn an_interface_answers_is_and_as() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // An interface is not on the single-inheritance chain `is` walks, so each type carries the
    // interfaces it implements — transitively, which is what makes the base of an interface, and
    // an interface of a superclass, answer as well as the one the class names itself.
    assert_eq!(
        run("interface Base\n\
             interface Derived : Base\n\
             open class Holder : Derived\n\
             class Sub : Holder()\n\
             class Other\n\
             fun main() {\n\
             \x20   val sub: Any = Sub()\n\
             \x20   println(sub is Derived)\n\
             \x20   println(sub is Base)\n\
             \x20   println(sub is Holder)\n\
             \x20   val other: Any = Other()\n\
             \x20   println(other is Base)\n\
             \x20   println((sub as Base) === sub)\n\
             \x20   println((other as? Base) == null)\n\
             }\n"),
        "true\ntrue\ntrue\nfalse\ntrue\ntrue\n"
    );
}

#[test]
fn an_override_that_needs_a_bridge_is_declined() {
    let Some(target) = host() else {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    };
    // `D4.foo(): Int` is what `D1.foo(): Any` gets here, and the two disagree about the machine:
    // one returns an unboxed integer, the other a reference. Pointing the interface's slot at the
    // inherited method would have a caller read an integer as a pointer. The JVM emits a bridge
    // for exactly this; until one is emitted here the file is declined rather than miscompiled.
    let (_, diagnostics) = compile(
        &[(
            "Main",
            "interface Boxed { fun foo(): Any }\n\
             open class Raw { fun foo(): Int = 42 }\n\
             class Both : Raw(), Boxed\n\
             fun main() { val b: Boxed = Both(); println(b.foo()) }\n",
        )],
        target,
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("a bridge method is needed")),
        "expected the backend to decline, got {diagnostics:?}"
    );
}

#[test]
fn a_value_class_answers_by_the_value_it_wraps() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // A `value class` is not a one-field class: Kotlin answers `equals`, `hashCode` and `toString`
    // by the value inside, where an ordinary class answers all three by identity. The JVM reaches
    // that by erasing the class to its underlying value; here the object stays and the three
    // members are synthesized instead, which is the same answer by a different road. A member
    // function still sees the wrapper, and a `String` inside compares by content.
    assert_eq!(
        run("@JvmInline value class Count(val n: Int) {\n\
             \x20   fun doubled(): Int = n * 2\n\
             }\n\
             @JvmInline value class Label(val text: String)\n\
             fun main() {\n\
             \x20   println(Count(1) == Count(1))\n\
             \x20   println(Count(1) == Count(2))\n\
             \x20   println(Count(1).hashCode() == Count(1).hashCode())\n\
             \x20   println(Count(1).toString())\n\
             \x20   println(Count(21).doubled())\n\
             \x20   println(Label(\"a\" + \"b\") == Label(\"ab\"))\n\
             \x20   println(Label(\"ab\").toString())\n\
             }\n"),
        "true\nfalse\ntrue\nCount(n=1)\n42\ntrue\nLabel(text=ab)\n"
    );
}

#[test]
fn an_operand_of_a_bounded_type_parameter_is_unboxed() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // `T : Int` is carried as a REFERENCE — a boxed `Int`, exactly as the JVM carries it — and
    // Kotlin's `+` is the primitive operator. Unifying the operands by their machine types without
    // opening the box first adds a pointer to an integer and prints the result as an answer: the
    // corpus's generic value classes are where that surfaced, but nothing about it is specific to
    // them, as the top-level function here shows.
    assert_eq!(
        run("var total = 0\n\
             fun <T : Int> add(a: T, b: T): Int = a + b\n\
             fun <T : Int> accumulate(v: T) { total += v }\n\
             class Holder<T : Int>(val v: T) {\n\
             \x20   fun twice(): Int = v + v\n\
             }\n\
             @JvmInline value class Wrapped<T : Int>(val v: T) {\n\
             \x20   fun plus(other: Wrapped<T>) = Wrapped(v + other.v)\n\
             }\n\
             fun main() {\n\
             \x20   println(add(1, 2))\n\
             \x20   accumulate(40)\n\
             \x20   accumulate(2)\n\
             \x20   println(total)\n\
             \x20   println(Holder(21).twice())\n\
             \x20   println(Wrapped(10).plus(Wrapped(20)).v)\n\
             }\n"),
        "3\n42\n42\n30\n"
    );
}

#[test]
fn a_lambda_becomes_the_fun_interface_it_is_converted_to() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // A SAM conversion changes the TYPE a function value wears, and a type is a table here: the
    // object holds its captures like any lambda, but a caller reaches it through the interface's
    // own member number. What it inherits from the interface matters too — a default method, and a
    // `kotlin.Any` member the interface overrides, both answer as an ordinary implementor's would.
    assert_eq!(
        run("fun interface Mapper {\n\
             \x20   fun map(n: Int): Int\n\
             \x20   fun twice(n: Int): Int = map(map(n))\n\
             }\n\
             fun interface Named { override fun toString(): String }\n\
             var seen = 0\n\
             fun interface Action { fun run() }\n\
             fun apply(m: Mapper, n: Int) = m.map(n)\n\
             fun render(value: Any) = value.toString()\n\
             fun main() {\n\
             \x20   println(apply({ it + 1 }, 20))\n\
             \x20   val by = 5\n\
             \x20   println(apply({ it + by }, 20))\n\
             \x20   val doubler = Mapper { it * 2 }\n\
             \x20   println(doubler.twice(3))\n\
             \x20   println(render(Named { \"named\" }))\n\
             \x20   val action = Action { seen = 7 }\n\
             \x20   action.run()\n\
             \x20   println(seen)\n\
             }\n"),
        "21\n25\n12\nnamed\n7\n"
    );
}

#[test]
fn converting_a_null_function_value_to_a_fun_interface_yields_null() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // Kotlin converts a nullable function value to a `fun interface` by yielding null when it is
    // null — not a wrapper around nothing, which would answer `!= null` and then call `invoke` on
    // the null inside.
    assert_eq!(
        run("var ran = 0\n\
             fun interface Runner { fun go() }\n\
             fun isNull(r: Runner?): Boolean {\n\
             \x20   if (r == null) return true\n\
             \x20   r.go()\n\
             \x20   return false\n\
             }\n\
             fun maybe(empty: Boolean): (() -> Unit)? = if (empty) null else {{ ran = ran + 1 }}\n\
             fun main() {\n\
             \x20   println(isNull(maybe(true)))\n\
             \x20   println(isNull(maybe(false)))\n\
             \x20   println(ran)\n\
             }\n"),
        "true\nfalse\n1\n"
    );
}

#[test]
fn an_inner_class_reaches_its_enclosing_instance() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // An `inner` class carries its outer instance in a field, stored before the superclass
    // constructor runs — Kotlin's own order, which a base-class `init` can observe. Reading
    // `this@Outer`, or an outer member without qualifying it, is a load of that field; writing one
    // reaches the same object the outer still holds, which is what the counter shows.
    assert_eq!(
        run("open class Base(val tag: String)\n\
             class Outer(var n: Int) {\n\
             \x20   inner class Inner : Base(\"inner\") {\n\
             \x20       fun sum(): Int = n + 1\n\
             \x20       fun qualified(): Int = this@Outer.n * 2\n\
             \x20       fun bump() { n = n + 5 }\n\
             \x20       fun label(): String = tag\n\
             \x20   }\n\
             \x20   inner class Deep {\n\
             \x20       inner class Deeper {\n\
             \x20           fun reach(): Int = this@Outer.n\n\
             \x20       }\n\
             \x20   }\n\
             }\n\
             fun main() {\n\
             \x20   val outer = Outer(20)\n\
             \x20   val inner = outer.Inner()\n\
             \x20   println(inner.sum())\n\
             \x20   println(inner.qualified())\n\
             \x20   println(inner.label())\n\
             \x20   inner.bump()\n\
             \x20   println(outer.n)\n\
             \x20   println(outer.Deep().Deeper().reach())\n\
             }\n"),
        "21\n40\ninner\n25\n25\n"
    );
}
