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
    let output = std::process::Command::new(&executable)
        .env_clear() // no PATH, no JAVA_HOME: nothing but the binary itself
        .output()
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
            let output = std::process::Command::new(&executable)
                .env_clear()
                .output()
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
fn structural_equality_on_references_is_declined_rather_than_compared_by_address() {
    let Some(target) = host() else {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    };
    // `"a" == "b"` is `equals`, not pointer identity. Comparing the two `i64`s would compile, link,
    // run, and answer the wrong question — the failure mode this backend refuses to have.
    let (_, diagnostics) = compile(
        &[(
            "Main",
            "fun same(a: String, b: String): Boolean = a == b\nfun main() { println(same(\"a\", \"a\")) }\n",
        )],
        target,
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("structural equality on references")),
        "expected the backend to decline, got {diagnostics:?}"
    );
    // Identity and null checks are pointer comparisons in Kotlin too, so those are emitted.
    assert_eq!(
        run("fun main() {\n\
             \x20   val s: String? = \"x\"\n\
             \x20   val n: String? = null\n\
             \x20   println(s == null)\n\
             \x20   println(n == null)\n\
             \x20   println(s === s)\n\
             }\n"),
        "false\ntrue\ntrue\n"
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
    // Lambdas are not implemented. The contract is that the backend SAYS so: emitting a partial
    // object that links and misbehaves would be far worse than refusing.
    let (artifacts, diagnostics) = compile(
        &[(
            "Main",
            "fun main() { val f = { x: Int -> x + 1 }; println(f(1)) }\n",
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
