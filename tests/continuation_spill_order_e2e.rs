//! The order a continuation class lays out its spill fields in.
//!
//! kotlinc groups them by kind — references `L$n`, ints `I$n`, longs `J$n` — and orders the GROUPS
//! by the kind that is spilled first, so the layout follows the code. krusty used one fixed order
//! for every method, which matched only when the code happened to agree with it.
//!
//! One `$N` class exists per suspend call site, and the generated HTTP clients in the corpus are
//! almost entirely suspend functions, so this is one of the most repeated differences there is.
use super::common;

/// An `Int` local reaches the suspension first, so the int group comes first.
#[test]
fn an_int_spilled_first_lays_out_its_group_first() {
    let src = "class Velarium\n\
               fun finish(first: Velarium, second: Velarium, number: Long): Velarium = first\n\
               suspend fun fetch(a: Int): Velarium = Velarium()\n\
               \n\
               suspend fun run(x: Int, y: Velarium, z: Long): Velarium {\n\
               \x20   val p = x + 1\n\
               \x20   val q = y\n\
               \x20   val r = z * 2\n\
               \x20   val first = fetch(p)\n\
               \x20   return finish(first, q, r)\n\
               }\n";
    let fields = spill_fields(src, "IntFirst", "IntFirstKt$run$1");
    assert_eq!(fields, ["I$0", "I$1", "L$0", "L$1", "J$0", "J$1"]);
}

/// The same locals in another order lay the groups out in that order — the point being that there
/// is no fixed table, which a single fixture could not show.
#[test]
fn a_reference_spilled_first_lays_out_its_group_first() {
    let src = "class Nacre\n\
               fun finish(first: Nacre, second: Nacre, number: Long): Nacre = first\n\
               suspend fun fetch(a: Int): Nacre = Nacre()\n\
               \n\
               suspend fun order(s: Nacre, n: Long, k: Int): Nacre {\n\
               \x20   val a = s\n\
               \x20   val b = n * 2\n\
               \x20   val c = k + 1\n\
               \x20   val got = fetch(c)\n\
               \x20   return finish(got, a, b)\n\
               }\n";
    let fields = spill_fields(src, "RefFirst", "RefFirstKt$order$1");
    assert_eq!(fields, ["L$0", "L$1", "J$0", "J$1", "I$0", "I$1"]);
}

/// Different suspension points can introduce different first kinds. Their field groups follow the
/// final body's suspension order, not the randomized iteration order of the scope lookup table.
#[test]
fn multiple_suspensions_keep_the_first_store_order() {
    let src = "class Cinder\n\
               fun keepNumber(first: Cinder, number: Int): Cinder = first\n\
               fun keepToken(first: Cinder, second: Cinder): Cinder = first\n\
               suspend fun fetch(a: Int): Cinder = Cinder()\n\
               \n\
               suspend fun choose(flag: Boolean): Cinder {\n\
               \x20   return if (flag) {\n\
               \x20       val n = 1\n\
               \x20       keepNumber(fetch(n), n)\n\
               \x20   } else {\n\
               \x20       val token = Cinder()\n\
               \x20       keepToken(fetch(0), token)\n\
               \x20   }\n\
               }\n";
    let fields = spill_fields(src, "MultipleSpills", "MultipleSpillsKt$choose$1");
    assert_eq!(fields, ["I$0", "L$0"]);
}

/// An inline function's parameters and extension receiver are locals of that expansion, so a
/// suspension inside the inlined body spills and names them independently of the caller's locals.
/// Nested `suspend inline` extensions make both the receiver role and one `$iv` suffix per
/// expansion observable.
#[test]
fn an_inline_expansions_parameters_and_receiver_are_named_spills() {
    let src = "class Nacre\n\
               class Velarium(val seed: Nacre)\n\
               \n\
               fun combine(left: Nacre, right: Nacre): Nacre = left\n\
               suspend fun fetch(value: Nacre): Nacre = value\n\
               \n\
               suspend inline fun <T> Velarium.call(token: Nacre, build: (Nacre) -> T): T {\n\
               \x20   val local = combine(token, seed)\n\
               \x20   val got = fetch(local)\n\
               \x20   return build(got)\n\
               }\n\
               \n\
               suspend inline fun Velarium.send(token: Nacre): Nacre {\n\
               \x20   val prepared = combine(token, seed)\n\
               \x20   return call(prepared) { combine(it, seed) }\n\
               }\n\
               \n\
               suspend fun run(host: Velarium, input: Nacre): Nacre = host.send(input)\n";
    let (slots, names) =
        debug_metadata_names(src, "InlineSpillNames", "InlineSpillNamesKt$run$1");
    assert_eq!(
        names,
        [
            "host",
            "input",
            "$this$send$iv",
            "token$iv",
            "prepared$iv",
            "$this$call$iv$iv",
            "token$iv$iv",
            "local$iv$iv",
        ],
        "both compilers name the same spilled locals"
    );
    // Field positions are not yet kotlinc's `L$0..L$7`: Krusty still spills three unnamed
    // expansion temporaries, which pushes the named fields apart. State that remaining divergence
    // explicitly so removing those temporaries changes this assertion rather than passing quietly.
    assert_eq!(
        slots,
        ["L$0", "L$1", "L$2", "L$3", "L$5", "L$8", "L$9", "L$10"],
        "Krusty's positions, still carrying three unnamed extra spills"
    );
}

/// A member inline EXTENSION binds both receivers at once, and Kotlin keeps them distinct: the
/// containing class's `this` and the receiver the callable extends are different values with
/// different debug identities. One receiver role cannot stand for the other — doing so collapsed
/// both to `$this$send$iv`.
#[test]
fn a_member_inline_extension_names_both_of_its_receivers() {
    let src = "class Box(val name: String)\n\
               \n\
               suspend fun fetch(v: String): String = v\n\
               \n\
               class Host(val prefix: String) {\n\
               \x20   suspend inline fun Box.send(url: String): String {\n\
               \x20       val local = prefix + url + name\n\
               \x20       val got = fetch(local)\n\
               \x20       return got\n\
               \x20   }\n\
               \x20   suspend fun run(b: Box, u: String): String = b.send(u)\n\
               }\n";
    let Some((_, names)) = debug_metadata_spills(src, "BothReceivers", "Host$run$1") else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    assert_eq!(
        names,
        ["b", "u", "this_$iv", "$this$send$iv", "url$iv", "local$iv"]
    );
}

/// Kotlin signs a context extension `(contexts…, receiver, values…)`, so the extension receiver is
/// the leading physical parameter only when the callable declares no `context(…)` clause. Selecting
/// it by position zero labelled the CONTEXT parameter as the receiver and left the real one to be
/// escaped as an ordinary value.
#[test]
fn a_context_parameter_does_not_displace_the_inline_receiver() {
    let src = "class Box(val name: String)\n\
               class Ctx(val tag: String)\n\
               \n\
               suspend fun fetch(v: String): String = v\n\
               \n\
               context(ctx: Ctx)\n\
               suspend inline fun Box.send(url: String): String {\n\
               \x20   val local = ctx.tag + url + name\n\
               \x20   val got = fetch(local)\n\
               \x20   return got + name\n\
               }\n\
               \n\
               context(ctx: Ctx)\n\
               suspend fun run(b: Box, u: String): String = b.send(u)\n";
    let Some((_, names)) = debug_metadata_spills(src, "ContextReceiver", "ContextReceiverKt$run$1")
    else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    assert_eq!(
        names,
        [
            "ctx",
            "b",
            "u",
            "ctx$iv",
            "$this$send$iv",
            "url$iv",
            "local$iv"
        ]
    );
}

/// `@DebugMetadata`'s `n`/`s` lists are neither the field layout nor a grouping by kind: kotlinc
/// hoists the REFERENCE spills and then keeps the order the locals were spilled in. Declaring a
/// `Long` between two `Int`s puts `J$0` between `I$0` and `I$1`, which no kind grouping produces —
/// and the field layout for the same method still groups the longs together, so one fixture shows
/// that the two orders are genuinely independent.
#[test]
fn debug_metadata_keeps_the_spill_order_after_the_references() {
    let src = "class Solivane\n\
               fun finish(first: Solivane, d: Long, c: Int, e: Long, n: Int): Solivane = first\n\
               suspend fun step(v: Solivane): Solivane = v\n\
               \n\
               suspend fun go(r: Solivane, d: Long, c: Int, e: Long): Solivane {\n\
               \x20   val n = c + 1\n\
               \x20   val first = step(r)\n\
               \x20   return finish(first, d, c, e, n)\n\
               }\n";
    let (slots, names) = debug_metadata_spills(src, "MetadataOrder", "MetadataOrderKt$go$1");
    assert_eq!(slots, ["L$0", "J$0", "I$0", "J$1", "I$1"]);
    assert_eq!(names, ["r", "d", "c", "e", "n"]);
    let fields = spill_fields(src, "MetadataOrderFields", "MetadataOrderFieldsKt$go$1");
    assert_eq!(
        fields,
        ["L$0", "J$0", "J$1", "I$0", "I$1"],
        "field layout groups by kind"
    );
}

/// The `s` and `n` arrays of a continuation's `@DebugMetadata`, from BOTH compilers — asserted
/// equal, and returned so the test can also state what they are.
fn debug_metadata_spills(src: &str, name: &str, class: &str) -> (Vec<String>, Vec<String>) {
    let (reference, krusty) = disassemble_verbose(name, src, class);
    let array = metadata_array;
    let want = (array(&reference, "s"), array(&reference, "n"));
    assert!(
        !want.0.is_empty(),
        "{class}: kotlinc records spilled locals"
    );
    assert_eq!(
        (array(&krusty, "s"), array(&krusty, "n")),
        want,
        "{class} @DebugMetadata spills"
    );
    want
}

/// The spilled-local names from both compilers, plus Krusty's physical fields. Use this while a
/// known physical-layout delta remains independently pinned.
fn debug_metadata_names(src: &str, name: &str, class: &str) -> (Vec<String>, Vec<String>) {
    let (reference, krusty) = disassemble_verbose(name, src, class);
    let want = metadata_array(&reference, "n");
    assert!(!want.is_empty(), "{class}: kotlinc records spilled locals");
    assert_eq!(
        metadata_array(&krusty, "n"),
        want,
        "{class} @DebugMetadata spilled local names"
    );
    (metadata_array(&krusty, "s"), want)
}

/// One complete `key=[…]` array from a `javap -v` annotation dump.
fn metadata_array(text: &str, key: &str) -> Vec<String> {
    let list = text
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix(key)?.strip_prefix("=["))
        .unwrap_or_else(|| panic!("DebugMetadata has no `{key}` array"));
    list.trim_end_matches(']')
        .split(',')
        .map(|entry| entry.trim().trim_matches('"').to_string())
        .collect()
}

/// The spill fields of one continuation class, in layout order, from BOTH compilers — asserted
/// equal, and returned so the test can also state what they are.
fn spill_fields(src: &str, name: &str, class: &str) -> Vec<String> {
    let (reference, krusty) = disassemble_both(name, src, class);
    let fields = |text: &str| {
        text.lines()
            .map(str::trim)
            .filter_map(|line| line.split_whitespace().last())
            .filter_map(|last| last.strip_suffix(';'))
            .filter(|name| {
                let mut parts = name.split('$');
                matches!(parts.next(), Some("L" | "I" | "J"))
                    && parts.next().is_some_and(|n| n.parse::<u32>().is_ok())
            })
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    let want = fields(&reference);
    assert!(!want.is_empty(), "{class}: kotlinc spills something");
    assert_eq!(fields(&krusty), want, "{class} spill field layout");
    want
}

/// The same source through both compilers, disassembled: `(kotlinc, krusty)`.
/// The one JVM target both sides compile for. A target difference forks codegen — indy string
/// concatenation, for one — so two differently-targeted classes are not an oracle for each other,
/// and a spill layout read off such a pair says nothing.
const TARGET: &str = "25";
/// `TARGET`'s class-file major version, which is how the in-process backend is told the same thing.
const TARGET_MAJOR: u16 = 69;

fn disassemble_both(name: &str, src: &str, class: &str) -> (String, String) {
    disassemble(name, src, class, false)
}

/// As [`disassemble_both`], with `javap -v` so the class's annotations are printed.
fn disassemble_verbose(name: &str, src: &str, class: &str) -> (String, String) {
    disassemble(name, src, class, true)
}

fn disassemble(name: &str, src: &str, class: &str, verbose: bool) -> (String, String) {
    let dir = common::scratch_dir()
        .unwrap_or_else(|| panic!("{name}: no scratch directory for spill-order differential"));
    let reference_dir = dir.join("ref");
    let krusty_dir = dir.join("out");
    std::fs::create_dir_all(&reference_dir)
        .unwrap_or_else(|error| panic!("{name}: create reference directory: {error}"));
    std::fs::create_dir_all(&krusty_dir)
        .unwrap_or_else(|error| panic!("{name}: create output directory: {error}"));
    let source = dir.join(format!("{name}.kt"));
    std::fs::write(&source, src)
        .unwrap_or_else(|error| panic!("{name}: write differential source: {error}"));
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        "-jvm-target".to_string(),
        TARGET.to_string(),
        source.to_string_lossy().into_owned(),
    ])
    .unwrap_or_else(|| panic!("{name}: reference kotlinc unavailable under the test harness"));
    assert_eq!(code, 0, "{name}: kotlinc failed: {stderr}");
    let classes = common::compile_in_process_metadata_cp_module_target(
        src,
        name,
        &[common::stdlib_jar()],
        "main",
        Some(TARGET_MAJOR),
    )
    .unwrap_or_else(|| panic!("{name}: krusty failed to compile"));
    for (internal, bytes) in &classes {
        let path = krusty_dir.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .unwrap_or_else(|error| panic!("{name}: create class directory: {error}"));
        }
        std::fs::write(path, bytes)
            .unwrap_or_else(|error| panic!("{name}: write emitted class: {error}"));
    }
    let reference_cp = reference_dir.to_string_lossy().into_owned();
    let mut reference_args = vec!["-p"];
    if verbose {
        reference_args.push("-v");
    }
    reference_args.extend(["-cp", reference_cp.as_str(), class]);
    let reference = common::javap(&reference_args)
        .unwrap_or_else(|| panic!("{name}: javap unavailable for the reference class"));
    let krusty_cp = krusty_dir.to_string_lossy().into_owned();
    let mut krusty_args = vec!["-p"];
    if verbose {
        krusty_args.push("-v");
    }
    krusty_args.extend(["-cp", krusty_cp.as_str(), class]);
    let krusty = common::javap(&krusty_args)
        .unwrap_or_else(|| panic!("{name}: javap unavailable for the emitted class"));
    let _ = std::fs::remove_dir_all(dir);
    (reference, krusty)
}
