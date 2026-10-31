//! `@file:JvmName("Facade") @file:JvmMultifileClass`: each source compiles to a synthetic part
//! `Facade__FooKt`, and the module writes one public facade forwarding to every part. The portable
//! shapes are box regressions (`tests/box_regressions/jvmMultifileClass*.kt`); these cover the
//! JVM-only ones byte for byte against kotlinc: a part's private functions are renamed after the
//! part, which function references, suspend continuations and their accessors all observe.

use super::common;

const FIRST: &str = "@file:JvmName(\"Shared\")\n\
    @file:JvmMultifileClass\n\
    package p\n\
    \n\
    private fun twice(value: Int) = value * 2\n\
    private fun scaled(value: Int = 3) = value * 10\n\
    \n\
    fun first(value: Int) = twice(value) + scaled()\n\
    fun reference(): (Int) -> Int = ::twice\n\
    fun defaulted(): Any = ::scaled\n\
    fun runnable(): Runnable = object : Runnable { override fun run() { twice(1) } }\n\
    \n\
    lateinit var late: String\n";

const SECOND: &str = "@file:JvmName(\"Shared\")\n\
    @file:JvmMultifileClass\n\
    package p\n\
    \n\
    private fun twice(value: Int) = value + value + 1\n\
    \n\
    fun second(value: Int) = twice(value)\n\
    fun thrice(value: Int): Int {\n\
    \x20   val add = { extra: Int -> twice(value) + extra }\n\
    \x20   return add(value - 1)\n\
    }\n\
    \n\
    private suspend fun step(value: Int): Int = value + 1\n\
    suspend fun steps(value: Int): Int = step(step(value))\n";

const CALLER: &str = "package p\n\
    \n\
    import kotlin.coroutines.*\n\
    \n\
    fun box(): String {\n\
    \x20   var stepped = 0\n\
    \x20   suspend { steps(40) }.startCoroutine(Continuation(EmptyCoroutineContext) { stepped = it.getOrThrow() })\n\
    \x20   late = \"late\"\n\
    \x20   runnable().run()\n\
    \x20   val joined = \"${first(1)} ${second(1)} ${thrice(1)} ${reference()(5)} $stepped $late\"\n\
    \x20   return if (joined == \"32 3 3 10 42 late\" && defaulted() == defaulted()) \"OK\" else joined\n\
    }\n";

const SOURCES: [(&str, &str); 3] = [
    ("first.kt", FIRST),
    ("second.kt", SECOND),
    ("Caller.kt", CALLER),
];

#[test]
fn multifile_parts_and_their_facade_match_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&SOURCES);
    assert_eq!(
        classes.reference.keys().collect::<Vec<_>>(),
        classes.krusty.keys().collect::<Vec<_>>(),
        "exact class inventory"
    );
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

#[test]
fn multifile_parts_run_through_their_facade() {
    common::expect_box_ok_files_with_stdlib(&SOURCES, "p.CallerKt");
}

/// A part of the facade `Shared` declaring `body`.
fn shared_part(body: &str) -> String {
    format!("@file:JvmName(\"Shared\")\n@file:JvmMultifileClass\npackage p\n{body}")
}

/// Compile `sources` with the krusty CLI and with kotlinc, and require both to reject the module
/// with exactly `errors`, kotlinc's unlocated ledger, and to write no class.
fn assert_rejected_like_kotlinc(sources: &[(&str, &str)], errors: &[&str]) {
    let rendered = errors
        .iter()
        .map(|error| format!("error: {error}\n"))
        .collect::<String>();
    let (reference_code, reference_stderr) = common::reference_compiler_run(sources, &[]);
    assert_eq!(
        (reference_code, reference_stderr.as_str()),
        (1, rendered.as_str())
    );

    let work = common::scratch_dir().expect("cannot allocate multifile clash fixture");
    let paths = common::write_fixture_sources(&work, sources);
    let out = work.join("out");
    let krusty = std::process::Command::new(common::krusty_binary())
        .arg("-d")
        .arg(&out)
        .arg("-cp")
        .arg(common::stdlib_jar())
        .args(&paths)
        .output()
        .expect("run krusty");
    let published = std::fs::read_dir(&out).map_or(0, |entries| entries.count());
    let _ = std::fs::remove_dir_all(&work);
    assert_eq!(
        (
            krusty.status.code(),
            String::from_utf8_lossy(&krusty.stdout).into_owned(),
            String::from_utf8_lossy(&krusty.stderr).into_owned(),
            published,
        ),
        (
            Some(1),
            String::new(),
            format!("{rendered}krusty: {} error(s)\n", errors.len()),
            0,
        )
    );
}

/// Two parts of one facade realize different functions under one JVM signature. Each part is
/// valid alone, so kotlinc reports the facade's platform declaration clash for the module, with
/// no location, and writes nothing.
#[test]
fn parts_declaring_one_jvm_signature_clash_on_their_facade() {
    let first = shared_part("@JvmName(\"same\")\nfun first(): Int = 1\n");
    let second = shared_part("@JvmName(\"same\")\nfun second(): Int = 2\n");
    assert_rejected_like_kotlinc(
        &[("First.kt", &first), ("Second.kt", &second)],
        &["platform declaration clash: The following declarations have the same JVM signature (same()I):\n    fun first(): Int defined in p\n    fun second(): Int defined in p"],
    );
}

/// A property accessor takes a facade slot like a function.
#[test]
fn a_part_accessor_clashes_with_another_parts_function() {
    let first = shared_part("@get:JvmName(\"same\")\nval x: Int get() = 1\n");
    let second = shared_part("@JvmName(\"same\")\nfun second(): Int = 2\n");
    assert_rejected_like_kotlinc(
        &[("First.kt", &first), ("Second.kt", &second)],
        &["platform declaration clash: The following declarations have the same JVM signature (same()I):\n    fun `<get-x>`(): Int defined in p\n    fun second(): Int defined in p"],
    );
}

/// Functions with defaults clash once, on their own signature; their `$default` stubs, which
/// clash as a consequence, are not reported again.
#[test]
fn defaulted_functions_clash_once_across_parts() {
    let first = shared_part("@JvmName(\"same\")\nfun first(a: Int = 1): Int = a\n");
    let second = shared_part("@JvmName(\"same\")\nfun second(a: Int = 2): Int = a\n");
    assert_rejected_like_kotlinc(
        &[("First.kt", &first), ("Second.kt", &second)],
        &["platform declaration clash: The following declarations have the same JVM signature (same(I)I):\n    fun first(a: Int = ...): Int defined in p\n    fun second(a: Int = ...): Int defined in p"],
    );
}

/// A `$default` stub takes a facade slot of its own, which an explicit function can claim.
#[test]
fn a_default_stub_clashes_with_another_parts_function() {
    let first =
        shared_part("@JvmName(\"same\\$default\")\nfun first(a: Int, m: Int, o: Any?): Int = a\n");
    let second = shared_part("@JvmName(\"same\")\nfun second(a: Int = 2): Int = a\n");
    assert_rejected_like_kotlinc(
        &[("First.kt", &first), ("Second.kt", &second)],
        &["platform declaration clash: The following declarations have the same JVM signature (same$default(IILjava/lang/Object;)I):\n    fun first(a: Int, m: Int, o: Any?): Int defined in p\n    fun `second$default`(a: Int = ..., `$mask0`: Int, `$handler`: Any?): Int defined in p"],
    );
}

/// Each clashing signature is one error, in the facade's part order, listing its declarations in
/// kotlinc's declaration order whatever order the files are given in.
#[test]
fn facade_clashes_follow_part_order_and_declaration_order() {
    let zeta = shared_part("@JvmName(\"same\")\nfun zeta(): Int = 1\n");
    let alpha = shared_part("@JvmName(\"same\")\nfun alpha(): Int = 2\n");
    let a = shared_part("@JvmName(\"other\")\nfun a(): Int = 3\n");
    let b = shared_part("@JvmName(\"other\")\nfun b(): Int = 4\n");
    assert_rejected_like_kotlinc(
        &[
            ("Fourth.kt", &b),
            ("Second.kt", &alpha),
            ("Third.kt", &a),
            ("First.kt", &zeta),
        ],
        &[
            "platform declaration clash: The following declarations have the same JVM signature (same()I):\n    fun alpha(): Int defined in p\n    fun zeta(): Int defined in p",
            "platform declaration clash: The following declarations have the same JVM signature (other()I):\n    fun a(): Int defined in p\n    fun b(): Int defined in p",
        ],
    );
}

/// Valid controls: one JVM name with different descriptors, a private function (the facade does
/// not forward it, and its part suffixes it), and one JVM name on two facades.
#[test]
fn distinct_facade_slots_compile_like_kotlinc() {
    let first = shared_part("@JvmName(\"same\")\nfun first(a: Int): Int = a\n");
    let second = shared_part("@JvmName(\"same\")\nfun second(): Int = 2\n");
    let hidden = shared_part(
        "@JvmName(\"same\")\nprivate fun hidden(): Int = 3\nfun useHidden() = hidden()\n",
    );
    let other_facade = "@file:JvmName(\"Other\")\n@file:JvmMultifileClass\npackage p\n@JvmName(\"same\")\nfun third(): Int = 3\n";
    let classes = common::classes_against_kotlinc_module(&[
        ("First.kt", &first),
        ("Second.kt", &second),
        ("Hidden.kt", &hidden),
        ("Third.kt", other_facade),
    ]);
    assert_eq!(
        classes.reference.keys().collect::<Vec<_>>(),
        classes.krusty.keys().collect::<Vec<_>>(),
        "exact class inventory"
    );
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}
