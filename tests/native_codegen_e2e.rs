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
fn an_unsupported_construct_is_declined_with_a_diagnostic() {
    let Some(target) = host() else {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    };
    // `try`/`catch` is not implemented — it needs an unwinder the runtime does not have. The
    // contract is that the backend SAYS so: emitting a partial object that links and misbehaves
    // would be far worse than refusing.
    let (artifacts, diagnostics) = compile(
        &[(
            "Main",
            "fun main() {\n\
             \x20   try { println(\"body\") } finally { println(\"cleanup\") }\n\
             }\n",
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
fn an_unsigned_integer_is_not_the_signed_one_sharing_its_bits() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // `UInt` is a value class over `Int`, and common lowering erases it to the `Int` it wraps.
    // These are the two questions that erasure answers wrongly if nothing else is done: a boxed
    // one must not BE an `Int`, and printing one must not print the signed number sharing its
    // bits. The whole family was declined until both could be answered.
    assert_eq!(
        run("fun same(x: UInt) = x as? Int\n             fun main() {\n             \x20   println(same(1u) == null)\n             \x20   println(UInt.MAX_VALUE)\n             \x20   println(ULong.MAX_VALUE)\n             \x20   println(UByte.MAX_VALUE)\n             }\n"),
        "true\n4294967295\n18446744073709551615\n255\n"
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
fn a_long_outside_the_cache_is_not_confused_with_one_inside_it() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // The cache slot is picked by the WHOLE value. Each `Long` here has a low word that lands in
    // -128..127 while the value does not — `Long.MIN_VALUE`'s low word is zero, `Long.MAX_VALUE`'s
    // is -1, and `1L shl 32` is zero again — so a slot chosen from the low word alone would hand
    // back whatever was cached for that small number, and the value would come out changed.
    assert_eq!(
        run("fun boxed(n: Long): Any = n\n\
             fun main() {\n\
             \x20   println(boxed(0L))\n\
             \x20   println(boxed(Long.MIN_VALUE))\n\
             \x20   println(boxed(-1L))\n\
             \x20   println(boxed(Long.MAX_VALUE))\n\
             \x20   println(boxed(1L shl 32))\n\
             \x20   println(boxed(0L) === boxed(0L))\n\
             \x20   println(boxed(Long.MIN_VALUE) == boxed(Long.MIN_VALUE))\n\
             \x20   println(boxed(0L) == boxed(Long.MIN_VALUE))\n\
             }\n"),
        "0\n-9223372036854775808\n-1\n9223372036854775807\n4294967296\ntrue\ntrue\nfalse\n"
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
fn a_slice_between_the_halves_of_one_character_fails_loudly() {
    let Some(target) = host() else {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    };
    // Kotlin lets a program cut a surrogate pair in half and answers with an unpaired surrogate.
    // UTF-8 has no encoding for one, so this runtime has no string to hand back — and handing back
    // a different text would be the worst of the three answers available.
    let (artifacts, diagnostics) = compile(
        &[(
            "Main",
            "fun main() {\n\
             \x20   val wide = \"a\\uD834\\uDD1Eb\"\n\
             \x20   println(\"before\")\n\
             \x20   println(wide.substring(0, 2))\n\
             }\n",
        )],
        target,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let objects = artifacts
        .iter()
        .map(|(_, bytes)| bytes.as_slice())
        .collect::<Vec<_>>();
    let image = krusty::native::link_program(&objects, target).expect("link");
    let scratch = Scratch::new("surrogate");
    let executable = scratch.path().join("program");
    std::fs::write(&executable, &image).expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let output = common::run_freshly_written(std::process::Command::new(&executable).env_clear())
        .expect("run");
    assert!(
        !output.status.success(),
        "a half-character slice must not continue"
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "before\n");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("inside a surrogate pair"),
        "stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_throw_a_program_wrote_stops_it_and_says_what_happened() {
    let Some(target) = host() else {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    };
    // The half that matters: a `TODO`, an `error` or a failed `require` must STOP the program. A
    // Kotlin exception is not catchable on this target — `try` declines whole — so the realization
    // is a diagnosable exit, and what it says has to name what the program asked for.
    for (source, expected) in [
        (
            "fun main() { println(\"before\"); TODO(\"the rest of it\") }\n",
            "An operation is not implemented: the rest of it",
        ),
        (
            "fun main() { println(\"before\"); TODO() }\n",
            "An operation is not implemented.",
        ),
        (
            "fun main() { println(\"before\"); error(\"nothing to do\") }\n",
            "nothing to do",
        ),
        (
            "fun main() { println(\"before\"); require(1 > 2) }\n",
            "Failed requirement.",
        ),
        (
            "fun main() { println(\"before\"); check(1 > 2) }\n",
            "Check failed.",
        ),
    ] {
        let (artifacts, diagnostics) = compile(&[("Main", source)], target);
        assert!(diagnostics.is_empty(), "{source}: {diagnostics:?}");
        let objects = artifacts
            .iter()
            .map(|(_, bytes)| bytes.as_slice())
            .collect::<Vec<_>>();
        let image = krusty::native::link_program(&objects, target).expect("link");
        let scratch = Scratch::new("throw");
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
        assert!(
            !output.status.success(),
            "{source}: a throw must not continue"
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "before\n",
            "{source}"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected),
            "{source}: stderr {:?} does not name {expected:?}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
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
    // `tailrec` is a promise about STACK, and the source wrote the modifier because the function
    // recurses past any stack — so accepting one the rewrite did not take means emitting a program
    // that dies on a guard page at a depth the source expects to survive. How deep a native stack
    // goes is the machine's business, and a gate must not depend on it, so the generator declines
    // these instead of compiling them into a crash.
    //
    // A member and an extension used to be on this list. They are rewritten now — the rewrite is a
    // question about the FRAME rather than about where a function was declared — and
    // `native_tailrec_e2e` asserts they run flat. What is left is the LOCAL `tailrec`, whose
    // captures take slots the step does not reassign.
    for (label, source) in [(
        "a local",
        "fun main() {\n\
         \x20   tailrec fun down(n: Int) { if (n > 0) down(n - 1) }\n\
         \x20   down(1000000)\n\
         }\n",
    )] {
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
    // `Boolean` is 1231 or 1237, and a `String` field compares by content.
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
fn a_floating_point_data_class_field_compares_by_its_bits() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // `Double.equals` is not `==`, and it disagrees with it in BOTH directions: `NaN` equals
    // itself, and the two zeroes are distinct. Comparing the reinterpreted bits is exactly Kotlin's
    // rule, where the comparison instruction answers the other way round on both of those values.
    // The hash follows from the same bits, which is what makes `NaN`'s hash a number at all.
    assert_eq!(
        run("data class Wide(val x: Double)\n\
             data class Narrow(val y: Float)\n\
             fun main() {\n\
             \x20   println(Wide(1.5) == Wide(1.5))\n\
             \x20   println(Wide(1.5) == Wide(2.5))\n\
             \x20   println(Wide(Double.NaN) == Wide(Double.NaN))\n\
             \x20   println(Wide(0.0) == Wide(-0.0))\n\
             \x20   println(Wide(Double.NaN).hashCode() == Wide(Double.NaN).hashCode())\n\
             \x20   println(Wide(0.0).hashCode() == Wide(-0.0).hashCode())\n\
             \x20   println(Narrow(Float.NaN) == Narrow(Float.NaN))\n\
             \x20   println(Narrow(0.0f) == Narrow(-0.0f))\n\
             \x20   println(Wide(1.5))\n\
             \x20   println(Narrow(2.5f))\n\
             }\n"),
        "true\nfalse\ntrue\nfalse\ntrue\nfalse\ntrue\nfalse\nWide(x=1.5)\nNarrow(y=2.5)\n"
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

#[test]
fn a_call_may_leave_arguments_out() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // A default is written in the CALLEE's frame and may read an earlier parameter, so it cannot
    // be evaluated at the call site. Each omission shape gets a wrapper that declares the callee's
    // whole frame, fills the missing slots in declaration order, and calls through. A defaulted
    // call on an open member still dispatches on its receiver: filling arguments does not decide
    // which implementation runs.
    assert_eq!(
        run("fun greet(name: String, greeting: String = \"hi \"): String = greeting + name\n\
             fun span(a: Int, b: Int = a + 1): Int = b - a\n\
             fun three(a: Int, b: Int = 2, c: Int = 3): Int = a * 100 + b * 10 + c\n\
             open class Base(val mark: String) {\n\
             \x20   open fun tag(text: String, suffix: String = mark): String = text + suffix\n\
             }\n\
             class Loud(mark: String) : Base(mark) {\n\
             \x20   override fun tag(text: String, suffix: String): String = text + suffix + \"!\"\n\
             }\n\
             data class Point(val x: Int, val y: Int)\n\
             fun main() {\n\
             \x20   println(greet(\"k\"))\n\
             \x20   println(greet(\"k\", \"yo \"))\n\
             \x20   println(span(10))\n\
             \x20   println(three(1))\n\
             \x20   println(three(1, c = 9))\n\
             \x20   val base: Base = Base(\".\")\n\
             \x20   println(base.tag(\"a\"))\n\
             \x20   val loud: Base = Loud(\".\")\n\
             \x20   println(loud.tag(\"a\"))\n\
             \x20   println(Point(1, 2).copy(y = 9))\n\
             }\n"),
        "hi k\nyo k\n1\n123\n129\na.\na.!\nPoint(x=1, y=9)\n"
    );
}

#[test]
fn an_override_the_tables_do_not_record_still_dispatches() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // A member EXTENSION's override has no entry in the frontend's override tables. Left at that,
    // it takes a slot of its own and a call through the base's type reaches the base's body — a
    // wrong answer with nothing to signal it. The IR still says what is needed: a declaration
    // WITHOUT `override` is listed as a fresh one, so a method absent from that list which matches
    // an inherited member by name and machine signature is that member's override.
    assert_eq!(
        run("open class Base {\n\
             \x20   open fun String.decorate(): String = \"base:\" + this\n\
             }\n\
             class Derived : Base() {\n\
             \x20   override fun String.decorate(): String = \"derived:\" + this\n\
             }\n\
             fun render(base: Base): String { with(base) { return \"x\".decorate() } }\n\
             fun main() {\n\
             \x20   println(render(Base()))\n\
             \x20   println(render(Derived()))\n\
             }\n"),
        "base:x\nderived:x\n"
    );
}

#[test]
fn a_class_may_have_more_than_one_constructor() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // A secondary constructor delegates and then runs its own body, and Kotlin's order is what is
    // being realized: a `this(…)` delegation reaches another constructor of the same class, which
    // runs the class's initializers, while a `super(…)` one runs them here — a class with no
    // primary constructor has nowhere else to run them. A chain of secondaries is the same rule
    // applied twice.
    assert_eq!(
        run("var order = \"\"\n\
             class Point(val x: Int, val y: Int) {\n\
             \x20   init { order += \"p\" }\n\
             \x20   constructor(both: Int) : this(both, both) { order += \"s\" }\n\
             \x20   constructor() : this(7) { order += \"t\" }\n\
             }\n\
             open class Base(val tag: String) { init { order += \"b\" } }\n\
             class Sub : Base {\n\
             \x20   init { order += \"i\" }\n\
             \x20   constructor(n: Int) : super(\"s\" + n) { order += \"c\" }\n\
             }\n\
             fun main() {\n\
             \x20   val square = Point(3)\n\
             \x20   println(square.x + square.y)\n\
             \x20   println(Point().x)\n\
             \x20   order = \"\"\n\
             \x20   println(Sub(1).tag)\n\
             \x20   println(order)\n\
             }\n"),
        "6\n7\ns1\nbic\n"
    );
}

#[test]
fn an_enum_class_is_built_whole_when_it_is_touched() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // Kotlin initializes an enum as a whole: every constant in declaration order, then the
    // companion — so a program that only ever mentions one constant still runs every constructor,
    // and in that order. Each constant is a singleton, so identity answers as equality does;
    // `toString` is the name, not the identity `kotlin.Any` would give; and `values()` hands back a
    // fresh array each call.
    assert_eq!(
        run("var order = \"\"\n\
             enum class Step(val weight: Int) {\n\
             \x20   FIRST(1), SECOND(2);\n\
             \x20   init { order += name + \"(\" + weight + \")\" }\n\
             \x20   companion object { init { order += \"|companion\" } }\n\
             }\n\
             fun describe(step: Step) = when (step) {\n\
             \x20   Step.FIRST -> \"one\"\n\
             \x20   Step.SECOND -> \"two\"\n\
             }\n\
             fun main() {\n\
             \x20   println(Step.SECOND.name)\n\
             \x20   println(order)\n\
             \x20   println(Step.SECOND.ordinal)\n\
             \x20   println(Step.FIRST.toString())\n\
             \x20   println(Step.FIRST === Step.FIRST)\n\
             \x20   println(Step.FIRST == Step.SECOND)\n\
             \x20   println(describe(Step.SECOND))\n\
             \x20   println(Step.values().size)\n\
             \x20   println(Step.values()[0].weight)\n\
             \x20   println(Step.valueOf(\"SECOND\").ordinal)\n\
             }\n"),
        "SECOND\nFIRST(1)SECOND(2)|companion\n1\nFIRST\ntrue\nfalse\ntwo\n2\n1\n1\n"
    );
}

#[test]
fn an_adapted_callable_reference_runs() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // A reference is ADAPTED when the function it names does not match the type it is used as: an
    // argument left to its default, a `vararg` given one element, a result discarded because the
    // expected type returns `Unit`. The checked lowering builds an adapter for each, and the
    // adapter is an ordinary function — so these need nothing of the generator beyond what any
    // reference needs, which is what removing the decline showed.
    assert_eq!(
        run(
            "fun greet(name: String, mark: String = \"!\"): String = name + mark\n\
             fun join(vararg parts: String): String {\n\
             \x20   var joined = \"\"\n\
             \x20   for (part in parts) joined += part\n\
             \x20   return joined\n\
             }\n\
             var counted = 0\n\
             fun count(): Int { counted = counted + 1; return counted }\n\
             class Box(val n: Int) { fun plus(extra: Int = 5): Int = n + extra }\n\
             fun apply(f: (String) -> String) = f(\"k\")\n\
             fun run(f: () -> Unit) { f() }\n\
             fun value(f: () -> Int) = f()\n\
             fun main() {\n\
             \x20   println(apply(::greet))\n\
             \x20   println(apply(::join))\n\
             \x20   run(::count)\n\
             \x20   println(counted)\n\
             \x20   println(value(Box(10)::plus))\n\
             }\n"
        ),
        "k!\nk\n1\n15\n"
    );
}

#[test]
fn a_class_declared_inside_a_function_runs() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // A local class is a class: the checked lowering lifts it to the file with its captures turned
    // into leading constructor parameters, so by the time the generator sees it there is nothing
    // local left. That is what removing the decline showed — the model and the constructor path
    // already handled it, and the decline was a claim about a difficulty that was not there.
    assert_eq!(
        run("fun make(base: Int): Int {\n\
             \x20   class Adder(val extra: Int) { fun sum() = base + extra }\n\
             \x20   return Adder(2).sum()\n\
             }\n\
             fun main() {\n\
             \x20   class Counter(val n: Int) { fun twice() = n * 2 }\n\
             \x20   class Box<T>(val v: T) { fun get(): T = v }\n\
             \x20   println(Counter(21).twice())\n\
             \x20   println(Box(42).get())\n\
             \x20   println(make(40))\n\
             }\n"),
        "42\n42\n42\n"
    );
}

#[test]
fn an_object_expression_implements_its_supertypes() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // `object : T { … }` is a class with one instance, built where it is written. Its constructor
    // is NOT its class's first declaration, so the checked lowering cannot recognize it as primary
    // and names it by its parameter list the way it names a secondary — which is why construction
    // now falls back to the primary when the list is the primary's own.
    assert_eq!(
        run("interface A { fun a(): Int }\n\
             interface B { fun b(): Int }\n\
             abstract class Base(val n: Int) { abstract fun twice(): Int }\n\
             interface P { fun p(): Int }\n\
             class Holder(val n: Int) { fun make(): P = object : P { override fun p() = n } }\n\
             fun supplier(n: Int): P = object : P { override fun p() = n + 1 }\n\
             fun main() {\n\
             \x20   val both = object : A, B {\n\
             \x20       override fun a() = 20\n\
             \x20       override fun b() = 22\n\
             \x20   }\n\
             \x20   println(both.a() + both.b())\n\
             \x20   val based = object : Base(21) { override fun twice() = n * 2 }\n\
             \x20   println(based.twice())\n\
             \x20   println(Holder(42).make().p())\n\
             \x20   println(supplier(41).p())\n\
             \x20   val counter = object {\n\
             \x20       var seen = 0\n\
             \x20       fun next(): Int { seen += 1; return seen }\n\
             \x20   }\n\
             \x20   counter.next()\n\
             \x20   println(counter.next() + 40)\n\
             }\n"),
        "42\n42\n42\n42\n42\n"
    );
}

#[test]
fn a_local_class_shares_the_mutable_locals_it_captures() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // `var a` that a local or anonymous class writes is ONE variable, not two: the class does not
    // get a copy, it gets the cell the enclosing function moved the variable into. The checked
    // lowering does the moving and marks which field carries the cell; `native::captures` is what
    // says that field holds a reference, and without it the write lands in a copy and the enclosing
    // function reads the value it started with.
    assert_eq!(
        run("fun box(): String {\n\
             \x20   var counted = 1\n\
             \x20   object { init { counted = 2 } }\n\
             \x20   var named = \"a\"\n\
             \x20   val setter = object { fun set() { named = \"b\" } }\n\
             \x20   setter.set()\n\
             \x20   var total = 0\n\
             \x20   class Bump { fun go() { total += 5 } }\n\
             \x20   Bump().go()\n\
             \x20   Bump().go()\n\
             \x20   return \"\" + counted + named + total\n\
             }\n\
             fun main() { println(box()) }\n"),
        "2b10\n"
    );
}

#[test]
fn a_local_classs_properties_keep_their_own_identities() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // A local class's body properties are numbered from the body, while the legacy source
    // coordinate numbers the constructor's `val` parameters first. Counting one against the other
    // bound every read of a property to the NEXT one — `a` read `b` — so `val b = a + 1` saw
    // nothing and `p.a` answered with `b`. Both backends were wrong in the same way, which is why
    // this reads the same values through krusty's JVM backend too (see the dual run in
    // `tests/common`).
    assert_eq!(
        run("fun box(): String {\n\
             \x20   class P(val n: Int) {\n\
             \x20       val a = n * 2\n\
             \x20       val b = a + 1\n\
             \x20       val c = b + 1\n\
             \x20   }\n\
             \x20   val p = P(3)\n\
             \x20   return \"\" + p.n + \" \" + p.a + \" \" + p.b + \" \" + p.c\n\
             }\n\
             fun main() { println(box()) }\n"),
        "3 6 7 8\n"
    );
}

#[test]
fn a_function_declared_inside_a_member_is_called_where_it_was_declared() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // A local function inside a member or an `init` block is lifted to a STATIC function owned by
    // the class. That owner is a JVM placement fact — there is no facade here for the function to
    // be placed differently from — so to this generator it is a function with a symbol, and the
    // call is direct.
    assert_eq!(
        run("class Counted {\n\
             \x20   val value: Int\n\
             \x20   init {\n\
             \x20       fun ten(): Int = 10\n\
             \x20       value = ten()\n\
             \x20   }\n\
             \x20   fun doubled(): Int {\n\
             \x20       fun twice(n: Int) = n * 2\n\
             \x20       return twice(value)\n\
             \x20   }\n\
             }\n\
             fun main() {\n\
             \x20   println(Counted().value)\n\
             \x20   println(Counted().doubled())\n\
             }\n"),
        "10\n20\n"
    );
}

#[test]
fn a_companion_constant_is_read_wherever_it_is_named() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // A `const val` in a companion is stored on the OUTER class on the JVM, and the IR records
    // that owner. Here the owner says nothing — a slot is a slot — and what it could have said
    // something about, WHEN the initializer runs, a `const` settles: the initializer is a
    // compile-time constant, so program-start initialization is indistinguishable from the
    // companion's own.
    assert_eq!(
        run("class Limits {\n\
             \x20   companion object {\n\
             \x20       const val MAX = 42\n\
             \x20       const val NAME = \"limit\"\n\
             \x20   }\n\
             }\n\
             object Solo { const val ONE = 1 }\n\
             fun main() {\n\
             \x20   println(Limits.MAX)\n\
             \x20   println(Limits.NAME)\n\
             \x20   println(Solo.ONE)\n\
             }\n"),
        "42\nlimit\n1\n"
    );
}

#[test]
fn a_secondary_constructor_may_delegate_to_the_root_class() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // A class with no primary constructor whose secondary delegates to `super()` reaches
    // `kotlin.Any`, which is not declared in any file — and needs nothing to be, because the root
    // declares no state and no constructor to run. The class's own initializers still run, folded
    // into this constructor's body by the checked lowering.
    assert_eq!(
        run("class Boxed {\n\
             \x20   val label: String\n\
             \x20   var seen = 0\n\
             \x20   init { seen = 1 }\n\
             \x20   constructor(text: String) { label = text }\n\
             \x20   constructor() : this(\"none\")\n\
             }\n\
             fun main() {\n\
             \x20   println(Boxed(\"here\").label)\n\
             \x20   println(Boxed().label)\n\
             \x20   println(Boxed().seen)\n\
             }\n"),
        "here\nnone\n1\n"
    );
}

#[test]
fn a_strings_length_counts_utf16_code_units() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // Kotlin's `String.length` counts UTF-16 CODE UNITS, and a krusty string holds UTF-8, so the
    // answer is neither the byte length nor the code-point count. A code point below U+0080 is one
    // byte and one unit; `é` is two bytes and one unit; `中` is three bytes and one unit; an emoji
    // above U+FFFF is four bytes and a SURROGATE PAIR, which is two units. A length reporting
    // bytes would answer 10 for the third line rather than 5, and one counting code points 4.
    assert_eq!(
        run("fun main() {\n\
             \x20   println(\"\".length)\n\
             \x20   println(\"abc\".length)\n\
             \x20   println(\"aé中🙂\".length)\n\
             \x20   println(\"ab\" + \"cé中🙂\")\n\
             \x20   println((\"ab\" + \"cé中🙂\").length)\n\
             }\n"),
        "0\n3\n5\nabcé中🙂\n7\n"
    );
}

#[test]
fn the_root_class_can_be_constructed() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // `Any()` is declared in no file and needs none: the root has no state and no constructor, so
    // the whole of constructing one is an object carrying the runtime's own `kotlin.Any` type. Two
    // of them are distinct, and each is itself.
    assert_eq!(
        run("fun main() {\n\
             \x20   val a = Any()\n\
             \x20   val b = Any()\n\
             \x20   println(a === a)\n\
             \x20   println(a === b)\n\
             \x20   println(a == b)\n\
             }\n"),
        "true\nfalse\nfalse\n"
    );
}

#[test]
fn a_declaration_that_stores_a_default_stores_nothing() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // `var flag = false` in a class body emits no store, and that is Kotlin's rule rather than an
    // optimization: the base constructor's call to `setup()` reaches the override and writes the
    // fields BEFORE the subclass's initializers would run, so a store here would overwrite what
    // the program just observed. A later `init { … }` assigning the same value is a different
    // statement and still runs, which is why the store's identity comes from the IR rather than
    // from its shape.
    assert_eq!(
        run("open class Base {\n\
             \x20   open fun setup() {}\n\
             \x20   init { setup() }\n\
             }\n\
             class Derived : Base() {\n\
             \x20   override fun setup() {\n\
             \x20       flag = true\n\
             \x20       count = 4\n\
             \x20       label = \"set\"\n\
             \x20   }\n\
             \x20   var flag = false\n\
             \x20   var count = 0\n\
             \x20   var label: String? = null\n\
             \x20   var reset = 7\n\
             \x20   init { reset = 0 }\n\
             }\n\
             fun main() {\n\
             \x20   val d = Derived()\n\
             \x20   println(d.flag)\n\
             \x20   println(d.count)\n\
             \x20   println(d.label)\n\
             \x20   println(d.reset)\n\
             }\n"),
        "true\n4\nset\n0\n"
    );
}

#[test]
fn a_construction_may_leave_arguments_out() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // A constructor's defaults are a function's problem with the object already in hand: a default
    // may read an EARLIER parameter, so it has to be evaluated in the constructor's own frame. Each
    // omission shape therefore gets a wrapper that declares that frame, fills the missing slots in
    // declaration order and calls the constructor — the allocation stays at the call site, because
    // allocating is not a default's to do. `class B : A()` reaches the same wrapper: a superclass
    // delegation that leaves arguments out is the same call written without parentheses of its own.
    assert_eq!(
        run(
            "open class Greeting(val name: String, val mark: String = \"!\", val full: String = name + mark)\n\
             open class Base(val label: String = \"base\")\n\
             class Derived : Base()\n\
             var built = 0\n\
             fun next(): String { built += 1; return built.toString() }\n\
             class Counted(val tag: String = next())\n\
             fun main() {\n\
             \x20   val one = Greeting(\"k\")\n\
             \x20   println(one.name + one.mark + one.full)\n\
             \x20   val two = Greeting(\"k\", \"?\")\n\
             \x20   println(two.full)\n\
             \x20   println(Greeting(\"k\", \"?\", \"given\").full)\n\
             \x20   println(Derived().label)\n\
             \x20   println(object : Base() {}.label)\n\
             \x20   println(Counted().tag + Counted().tag)\n\
             }\n"
        ),
        "k!k!\nk?\ngiven\nbase\nbase\n12\n"
    );
}

#[test]
fn an_enum_with_no_constants_is_still_an_enum() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // `enum class Empty` declares no constants and is a real Kotlin declaration all the same.
    // Enum-ness was being read off the CONSTANT LIST, which makes this one indistinguishable from
    // a class that is not an enum: it was skipped when slots were declared and then panicked the
    // moment `values()` looked itself up. The answers fall out once it is registered — a
    // zero-length array, and a `valueOf` with no candidate to find.
    assert_eq!(
        run("enum class Empty\n\
             fun main() {\n\
             \x20   println(Empty.values().size)\n\
             \x20   println(Empty.values() === Empty.values())\n\
             }\n"),
        "0\nfalse\n"
    );
}

#[test]
fn an_enum_constant_may_have_a_body() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // `ADD { … }` is not the enum: it is an instance of a synthesized subclass, which is how it
    // overrides a member and how it can declare state of its own. Common IR names that subclass on
    // the entry and records only the USER parameter types on it, because the JVM's enum ABI gives
    // its constructor a leading `(String name, int ordinal)` that is a realization rather than a
    // Kotlin fact. This generator stores the name and ordinal itself, so the subclass's
    // constructor takes exactly those user parameters and passes them to the enum's — and
    // everything else about a constant, `values()`, `valueOf`, `toString`, `is`, is unchanged,
    // because the subclass inherits the enum's whole layout and table.
    //
    // `Plain.B.name + Plain.B.ordinal` is here for a gap this found in passing: `name` and
    // `ordinal` belong to `kotlin.Enum`, which no file declares, so the checked property table had
    // nothing to say about their types and a concatenation of one could not be typed.
    assert_eq!(
        run("enum class Op(val tag: String) {\n\
             \x20   ADD(\"+\") { override fun apply(a: Int, b: Int) = a + b },\n\
             \x20   MUL(\"*\") {\n\
             \x20       val scale = 2\n\
             \x20       override fun apply(a: Int, b: Int) = a * b * scale\n\
             \x20   };\n\
             \x20   abstract fun apply(a: Int, b: Int): Int\n\
             \x20   fun described(): String = tag + name + ordinal\n\
             }\n\
             enum class Plain { A, B }\n\
             fun main() {\n\
             \x20   println(Op.ADD.apply(2, 3))\n\
             \x20   println(Op.MUL.apply(2, 3))\n\
             \x20   println(Op.MUL.described())\n\
             \x20   println(Op.valueOf(\"ADD\").apply(1, 1))\n\
             \x20   for (op in Op.values()) println(op.toString())\n\
             \x20   println(Op.ADD is Op)\n\
             \x20   println(Plain.B.name + Plain.B.ordinal)\n\
             }\n"),
        "5\n12\n*MUL1\n2\nADD\nMUL\ntrue\nB1\n"
    );
}

#[test]
fn floating_point_values_render_as_kotlin_renders_them() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // Kotlin's `toString` for a floating-point value is the SHORTEST decimal that reads back as
    // exactly that value, printed plainly while the magnitude is in `[10^-3, 10^7)` and as
    // `d.dddEn` outside it. The runtime works that out with exact integer arithmetic
    // (`krusty_fp.c`); every value here is one where a shorter algorithm gives a different answer —
    // `0.1` has no exact binary form, `0.1 + 0.2` needs all seventeen digits, `9999999.0` and
    // `1.0E7` sit either side of the format boundary, and `Double.MIN_VALUE` is the case where the
    // shortest decimal is deliberately NOT what Kotlin prints.
    assert_eq!(
        run(
            "fun main() {\n\
             \x20   println(0.0)\n\
             \x20   println(-0.0)\n\
             \x20   println(0.1)\n\
             \x20   println(0.1 + 0.2)\n\
             \x20   println(1.0 / 3.0)\n\
             \x20   println(9999999.0)\n\
             \x20   println(1.0E7)\n\
             \x20   println(0.001)\n\
             \x20   println(1.0E-4)\n\
             \x20   println(Double.MIN_VALUE)\n\
             \x20   println(Double.MAX_VALUE)\n\
             \x20   println(1.0 / 0.0)\n\
             \x20   println(0.0 / 0.0)\n\
             \x20   println(1.0f / 3.0f)\n\
             \x20   println(Float.MIN_VALUE)\n\
             \x20   val boxed: Any = 2.5\n\
             \x20   println(boxed)\n\
             \x20   println(\"x=\" + 1.5 + \" y=\" + 2.5f)\n\
             }\n"
        ),
        "0.0\n-0.0\n0.1\n0.30000000000000004\n0.3333333333333333\n9999999.0\n1.0E7\n0.001\n1.0E-4\n4.9E-324\n1.7976931348623157E308\nInfinity\nNaN\n0.33333334\n1.4E-45\n2.5\nx=1.5 y=2.5\n"
    );
}

#[test]
fn what_kotlin_asks_of_a_value_directly() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // Two questions Kotlin asks of a value directly. `is Double` needs the runtime's own type for
    // one, which it has now that a floating-point value has a box; `isNaN` and its siblings are
    // one comparison each, emitted here rather than called into the runtime so the operand is
    // never boxed to ask about its bits.
    //
    // `'A' + 1` is here because those two made it reachable: Kotlin declares `Char.plus(Int): Char`
    // and only `Char.minus(Char): Int`, so the boxed result is a `Char` holding `'B'` — where the
    // rule for the OTHER narrow types, that arithmetic on them is `Int` arithmetic, would have
    // boxed an `Int` holding 66.
    assert_eq!(
        run("fun main() {\n\
             \x20   val boxed: Any = 'A' + 1\n\
             \x20   println(boxed)\n\
             \x20   println(boxed is Char)\n\
             \x20   println(boxed == 'B')\n\
             \x20   val gap: Any = 'B' - 'A'\n\
             \x20   println(gap is Int)\n\
             \x20   println(('z' - 1).toString())\n\
             \x20   val d: Any = 1.0\n\
             \x20   val f: Any = 1.0f\n\
             \x20   println(d is Double)\n\
             \x20   println(f is Float)\n\
             \x20   println(d is Float)\n\
             \x20   println((0.0 / 0.0).isNaN())\n\
             \x20   println((1.0 / 0.0).isInfinite())\n\
             \x20   println(1.0.isFinite())\n\
             \x20   println((1.0 / 0.0).isFinite())\n\
             \x20   println((0.0f / 0.0f).isNaN())\n\
             }\n"),
        "B\ntrue\ntrue\ntrue\ny\ntrue\ntrue\nfalse\ntrue\ntrue\ntrue\nfalse\ntrue\n"
    );
}
