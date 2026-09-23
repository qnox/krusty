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
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(classpath.clone())
            .expect("JVM provider initialization"),
    );
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
    let provider: std::rc::Rc<dyn krusty::libraries::SemanticPlatform> = std::rc::Rc::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(classpath)
            .expect("JVM provider initialization"),
    );
    let backend = CraneliftBackend::new(provider, target);
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
    // out-of-range field fails the link, not the run. The program calls, loops, recurses and
    // builds a string, so the calls into the runtime are cross-compiled too, not only arithmetic.
    let mut linked = Vec::new();
    for &target in NativeTarget::ALL {
        if !krusty::native::can_link(target) {
            eprintln!("skipping {target}: no prebuilt runtime in this build");
            continue;
        }
        let (artifacts, diagnostics) = compile(
            &[(
                "Main",
                "fun greet(who: String, n: Long): String = \"Hello, $who! $n\"\n\
                 fun fib(n: Int): Int = if (n < 2) n else fib(n - 1) + fib(n - 2)\n\
                 fun main() {\n\
                 \x20   var total = 0L\n\
                 \x20   var i = 0\n\
                 \x20   while (i < 10) {\n\
                 \x20       i = i + 1\n\
                 \x20       if (i % 2 == 0) continue\n\
                 \x20       total = total + fib(i)\n\
                 \x20   }\n\
                 \x20   println(greet(\"world\", total))\n\
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
    // `kotlin.Result` stands in here, because it is the largest remaining cluster on the backlog.
    // The contract under test is not WHICH construct is refused but that the backend SAYS so —
    // emitting a partial object that links and misbehaves would be far worse than refusing.
    //
    // It has been `try`/`finally`, then an unsigned RANGE, then an unsigned `downTo` as a loop
    // HEADER, then `downTo` as a VALUE — each of which now lowers. A decline test has to name
    // something still declined, so this moves as the backlog does, and it moving is the point:
    // each time, the thing it named became supported.
    let (artifacts, diagnostics) = compile(
        &[(
            "Main",
            "fun main() {\n\
             \x20   println(runCatching { 1 }.getOrNull())\n\
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
    // `!!` raises Kotlin's `NullPointerException`, which a program may CATCH; uncaught, it reaches
    // the entry and is reported there. kotlinc gives that exception no message, and neither does
    // this — `null as T` is the one that names a type, and the two are observably different.
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("kotlin.NullPointerException"),
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

#[test]
fn a_branch_ending_in_a_local_it_declares_still_has_a_value() {
    // A `when` types itself BEFORE lowering any arm, to know whether its merge block carries a
    // value. A branch spliced from an inline function ends in a local that branch declares, and
    // the slot map that answers a local's type is a lowering artifact — empty until the declaring
    // statement is emitted. So the branch had no type, the `when` typed as no-value, every arm was
    // lowered as a statement, and the destination read zero.
    //
    // `Array(n) { … }` is how that shape arrives in practice: it is an inline function, so
    // `if (c) Array(2) { 1 } else Array(5) { 1 }` answered an array of size 0 while the same
    // constructor answered 2 outside a branch. Nothing about the defect is specific to arrays.
    common::expect_box_ok_with_stdlib(
        "fun box(): String {\n\
         \x20   val c = true\n\
         \x20   val direct = Array(2) { 1 }\n\
         \x20   val inIf = if (c) Array(2) { 1 } else Array(5) { 1 }\n\
         \x20   val inWhen = when { c -> Array(2) { 1 }; else -> Array(5) { 1 } }\n\
         \x20   val sized = Array(if (c) 2 else 5) { 1 }\n\
         \x20   val counted = if (c) (1..3).map { it * 2 } else emptyList()\n\
         \x20   if (direct.size != 2) return \"fail direct: ${direct.size}\"\n\
         \x20   if (inIf.size != 2) return \"fail inIf: ${inIf.size}\"\n\
         \x20   if (inWhen.size != 2) return \"fail inWhen: ${inWhen.size}\"\n\
         \x20   if (sized.size != 2) return \"fail sized: ${sized.size}\"\n\
         \x20   if (counted.size != 3) return \"fail counted: ${counted.size}\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "P",
    );
}

#[test]
fn a_break_inside_an_expression_leaves_the_loop_with_the_expression_half_built() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // `x + break` jumps out while the concatenation is still being assembled, so the store and
    // the loop's back edge never happen and the block opened for them ends in whatever the
    // abandoned expression had already materialized — a constant, with no terminator after it.
    // Cranelift rejects such a block, so the whole function was refused. The value is the point
    // as much as the compile: `x` must keep the value it had, the loop must be left once, and
    // the `continue` arm must go round exactly as many times as it is told.
    assert_eq!(
        run("fun main() {\n\
             \x20   var x = \"OK\"\n\
             \x20   while (true) {\n\
             \x20       x = x + break\n\
             \x20   }\n\
             \x20   println(x)\n\
             \x20   var seen = 0\n\
             \x20   var i = 0\n\
             \x20   while (i < 3) {\n\
             \x20       i = i + 1\n\
             \x20       val skip = \"\" + if (i == 2) continue else \"\"\n\
             \x20       seen = seen + skip.length + 1\n\
             \x20   }\n\
             \x20   println(seen)\n\
             }\n"),
        "OK\n2\n"
    );
}

#[test]
fn a_break_in_an_expression_under_a_finally_still_runs_the_finally() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // The same shape with a `finally` between the jump and the loop it leaves. The `continue` in
    // the body is abandoned mid-expression exactly as above, and on its way out it still has to
    // run the `finally` — whose own `break` is abandoned mid-expression in turn.
    assert_eq!(
        run("fun main() {\n\
             \x20   var x = \"OK\"\n\
             \x20   var ran = 0\n\
             \x20   while (true) {\n\
             \x20       try {\n\
             \x20           x = x + continue\n\
             \x20       } finally {\n\
             \x20           ran = ran + 1\n\
             \x20           x = x + break\n\
             \x20       }\n\
             \x20   }\n\
             \x20   println(x)\n\
             \x20   println(ran)\n\
             }\n"),
        "OK\n1\n"
    );
}

#[test]
fn a_result_declared_narrower_than_the_value_it_returns_is_converted() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // A value typed by a type PARAMETER is carried as a reference, whatever the parameter is
    // bounded by. `fun <T : Int> foo(x: T): Int = x` returns that reference where the declaration
    // says a machine integer, so the `return` has to unbox — it used to hand the pointer straight
    // back, which the code generator's verifier refused (`result 0 has type i64, must match …
    // i32`). The nullable bound is the same shape one level out.
    assert_eq!(
        run("fun <T : Int> width(x: T): Int = x\n\
             fun <T : Long> wide(x: T): Long = x\n\
             fun <T : Boolean> truth(x: T): Boolean = x\n\
             fun main() {\n\
             \x20   println(width(17))\n\
             \x20   val computed = 3 + 4\n\
             \x20   println(width(computed))\n\
             \x20   println(wide(9000000000L))\n\
             \x20   println(truth(true))\n\
             }\n"),
        "17\n7\n9000000000\ntrue\n"
    );
}

#[test]
fn a_floating_point_comparison_stays_ieee_when_an_operand_arrives_boxed() {
    // Kotlin compares two floating-point operands by IEEE rules whenever both static types are the
    // floating-point type itself — including through a type parameter bounded by it and through its
    // nullable form. `NaN == NaN` is then false and `0.0 == -0.0` true, and BOTH verdicts reverse
    // once an operand widens to `Any`, where `equals` answers the total order instead. A boxed
    // operand does not change which rule applies, so the box is opened rather than handed to the
    // total order; `null` is not a number and is answered before any unboxing.
    //
    // Run on both backends and REQUIRED of the native one: a decline is the defect this pins, and
    // the ordinary cross-check treats a decline as a skip.
    let source = "fun <T : Double> sameDouble(d: Double, v: T): Boolean = d == v\n\
         fun <T : Float> sameFloat(f: Float, v: T): Boolean = f == v\n\
         fun box(): String {\n\
         \x20   val nan = Double.NaN\n\
         \x20   if (sameDouble(nan, nan)) return \"fail: NaN == NaN through a type parameter\"\n\
         \x20   if (!sameDouble(0.0, -0.0)) return \"fail: 0.0 != -0.0 through a type parameter\"\n\
         \x20   if (sameFloat(Float.NaN, Float.NaN)) return \"fail: Float NaN == NaN\"\n\
         \x20   if (!sameFloat(0.0f, -0.0f)) return \"fail: Float 0.0 != -0.0\"\n\
         \x20   val widened: Any = nan\n\
         \x20   if (widened != widened) return \"fail: a widened NaN is not itself\"\n\
         \x20   val absent: Double? = null\n\
         \x20   val quiet: Double? = nan\n\
         \x20   if (quiet == quiet) return \"fail: NaN? == NaN?\"\n\
         \x20   if (quiet == absent) return \"fail: NaN? == null\"\n\
         \x20   if (absent != absent) return \"fail: null != null\"\n\
         \x20   return \"OK\"\n\
         }\n";
    common::expect_box_ok_with_stdlib(source, "Ieee");
    common::expect_native_box(source, "Ieee", "OK");
}

#[test]
fn a_do_while_condition_reads_what_its_body_declares() {
    // Kotlin scopes a `do`-block's locals into the `while` that closes it, so the condition is the
    // one place a loop test may read a local the BODY declares. The test was lowered before the
    // body whatever the loop's shape, so those reads found a slot that did not exist yet. The
    // block it fills is the same; only when it is filled changes.
    let source = "fun box(): String {\n\
         \x20   var x = 0\n\
         \x20   do {\n\
         \x20       x++\n\
         \x20       val limit = x + 5\n\
         \x20   } while (limit < 10)\n\
         \x20   if (x != 5) return \"fail 1: $x\"\n\
         \x20   var rounds = 0\n\
         \x20   do {\n\
         \x20       val left = \"X\"\n\
         \x20       val right = \"Y\"\n\
         \x20       rounds++\n\
         \x20   } while (left + right != \"XY\")\n\
         \x20   if (rounds != 1) return \"fail 2: $rounds\"\n\
         \x20   return \"OK\"\n\
         }\n";
    common::expect_box_ok_with_stdlib(source, "DoWhileScope");
    common::expect_native_box(source, "DoWhileScope", "OK");
}

/// A collection declines by the declaration it names rather than reaching a runtime function
/// through a table that happens to know the name. A list, a map and a range are the runtime's
/// objects, and nothing here yet lowers a call that makes or reads one.
#[test]
fn a_collection_declines_by_the_declaration_it_names() {
    common::expect_native_decline(
        "fun box(): String {\n\
         \x20   val xs = listOf(\"O\", \"K\")\n\
         \x20   return \"OK\"\n\
         }\n",
        "CollectionDeclines",
        "listOf",
    );
}

/// A function value declines by the node that makes it. A lambda is an object with a body of its
/// own and a place in the function-type slot, and nothing here yet builds one.
#[test]
fn a_function_value_declines_by_the_node_that_makes_it() {
    common::expect_native_decline(
        "fun box(): String {\n\
         \x20   val f = { \"OK\" }\n\
         \x20   return \"OK\"\n\
         }\n",
        "FunctionValueDeclines",
        "Lambda",
    );
}

/// A class declines before any of its file is lowered. Its layout, its descriptor and every
/// member reached through it are one piece, and none of that is here yet.
#[test]
fn a_class_declines_by_its_name() {
    common::expect_native_decline(
        "class Box(val s: String)\n\
         fun box(): String = Box(\"OK\").s\n",
        "ClassDeclines",
        "a class (`Box`)",
    );
}

/// A top-level property declines too: its storage and its initializer run are the classes tier's.
#[test]
fn a_top_level_property_declines() {
    common::expect_native_decline(
        "val greeting = \"OK\"\n\
         fun box(): String = greeting\n",
        "TopLevelPropertyDeclines",
        "a top-level property",
    );
}

/// Kotlin's other entry point declines by name. This target does not pass a program its
/// arguments yet, and without the decline the file lowers with no entry at all and the link fails
/// on a symbol that says nothing about `main`.
#[test]
fn a_main_taking_its_arguments_declines() {
    common::expect_native_decline(
        "fun main(args: Array<String>) {\n\
         \x20   println(\"OK\")\n\
         }\n",
        "MainWithArguments",
        "a `main` that takes its arguments",
    );
}
