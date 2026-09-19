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

/// A spliced lambda's own VALUE parameters are locals of the splice and keep their source names, so
/// a suspension inside the body spills them under those names — `p` for a lambda written at source
/// level, and one `$iv` per enclosing expansion for one written inside an inline function.
///
/// krusty bound them the way it bound inline parameters: the caller's slot when the argument was
/// already a local read, an unnamed temp otherwise. Either way the parameter had no name, so it was
/// absent from `@DebugMetadata` while still consuming its field position.
#[test]
fn a_spliced_lambdas_value_parameters_keep_their_names() {
    let src = "suspend fun step(v: String): String = v\n\
               \n\
               inline fun <T> tagged(tag: String, block: (String) -> T): T {\n\
               \x20   val prefix = \"[\" + tag + \"]\"\n\
               \x20   val suffix = tag + \"!\"\n\
               \x20   return block(prefix + suffix)\n\
               }\n\
               \n\
               suspend fun run(tag: String): String {\n\
               \x20   return tagged(tag) { p -> step(p) + p }\n\
               }\n";
    let (slots, names) = debug_metadata_names(src, "LambdaSpillNames", "LambdaSpillNamesKt$run$1");
    assert_eq!(
        names,
        ["tag", "tag$iv", "prefix$iv", "suffix$iv", "p"],
        "both compilers name the same spilled locals"
    );
    // As with the inline parameters, the POSITIONS are not yet kotlinc's `L$0..L$4`: one unnamed
    // temporary of the expansion still takes a field krusty's `L$2`. Stated rather than hidden.
    assert_eq!(
        slots,
        ["L$0", "L$1", "L$3", "L$4", "L$5"],
        "krusty's positions, still carrying one unnamed extra spill"
    );
}

/// An inline function's PARAMETERS and extension RECEIVER are locals of the expansion, so a
/// suspension inside the inlined body spills them and `@DebugMetadata` names them — `url$iv`,
/// `$this$send$iv`, with one `$iv` per nesting level.
///
/// krusty reused the caller's slot whenever the argument was already a local read, which is the
/// common case, so the parameter had no identity of its own: it was never spilled and never named.
/// The extension receiver arrives as a leading physical parameter whose recorded name already
/// carries the receiver spelling, so it has to be handed to debug naming in the receiver ROLE or the
/// `$`s in it get escaped to `_u24`.
///
/// This shape — nested `suspend inline` extensions — is what the corpus is built from, a client
/// method calling `post`/`request`/`body`.
#[test]
fn an_inline_expansions_parameters_and_receiver_are_named_spills() {
    let src = "class Box(val name: String)\n\
               \n\
               suspend fun fetch(v: String): String = v\n\
               \n\
               suspend inline fun <T> Box.call(url: String, build: (String) -> T): T {\n\
               \x20   val local = url + name\n\
               \x20   val got = fetch(local)\n\
               \x20   return build(got)\n\
               }\n\
               \n\
               suspend inline fun Box.send(url: String): String {\n\
               \x20   val prepared = url + \"?\"\n\
               \x20   return call(prepared) { it + name }\n\
               }\n\
               \n\
               suspend fun run(b: Box, u: String): String = b.send(u)\n";
    let (slots, names) = debug_metadata_names(src, "InlineSpillNames", "InlineSpillNamesKt$run$1");
    assert_eq!(
        names,
        [
            "b",
            "u",
            "$this$send$iv",
            "url$iv",
            "prepared$iv",
            "$this$call$iv$iv",
            "url$iv$iv",
            "local$iv$iv",
        ],
        "both compilers name the same spilled locals"
    );
    // The field POSITIONS are not yet kotlinc's `L$0..L$7`: krusty still spills three unnamed
    // temporaries of the expansion that kotlinc does not, which pushes the named ones apart. That
    // is the remaining half of this divergence and it is stated here rather than hidden, so closing
    // it fails this assertion and updates it.
    assert_eq!(
        slots,
        ["L$0", "L$1", "L$2", "L$3", "L$5", "L$8", "L$9", "L$10"],
        "krusty's positions, still carrying three unnamed extra spills"
    );
}

/// More than one value parameter, so the lookup is positional rather than trivially the only one.
#[test]
fn a_spliced_lambda_names_each_of_its_value_parameters() {
    let src = "suspend fun step(v: String): String = v\n\
               \n\
               inline fun <T> pair(block: (String, Int) -> T): T = block(\"x\", 1)\n\
               \n\
               suspend fun run(seed: String): String {\n\
               \x20   return pair { text, count -> step(seed + text) + count }\n\
               }\n";
    let (slots, names) =
        debug_metadata_names(src, "LambdaTwoParameters", "LambdaTwoParametersKt$run$1");
    assert_eq!(names, ["seed", "text", "count"]);
    // As elsewhere in this file, the positions still carry an unnamed temporary of the expansion
    // that kotlinc does not have. Stated rather than hidden; closing it fails this and updates it.
    assert_eq!(slots, ["L$0", "L$2", "I$0"]);
}

/// A lambda declared INSIDE an inline function is spliced one expansion deeper, so its parameter
/// gains an inline frame where a lambda written at the call site does not. The shallow case alone
/// cannot show that the depth comes from the enclosing expansion rather than from the splice.
#[test]
fn a_lambda_declared_inside_an_inline_function_gains_its_frame() {
    let src = "suspend fun step(v: String): String = v\n\
               \n\
               inline fun <T> feed(block: (String) -> T): T = block(\"x\")\n\
               \n\
               suspend inline fun outer(prefix: String): String = feed { p ->\n\
               \x20   val got = step(prefix + p)\n\
               \x20   got + p\n\
               }\n\
               \n\
               suspend fun run(seed: String): String = outer(seed)\n";
    let (slots, names) =
        debug_metadata_names(src, "LambdaInsideInline", "LambdaInsideInlineKt$run$1");
    assert_eq!(names, ["seed", "prefix$iv", "p$iv"]);
    assert_eq!(slots, ["L$0", "L$1", "L$4"]);
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
    let (_, names) = debug_metadata_spills(src, "BothReceivers", "Host$run$1");
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
    let (_, names) = debug_metadata_spills(src, "ContextReceiver", "ContextReceiverKt$run$1");
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

/// An inline expansion whose ONLY return is its tail needs neither a result local nor the loop that
/// carries a non-local return out of it. kotlinc leaves that value on the operand stack; krusty
/// always built `var result = zero; loop@ while (true) { body; break@loop }; result`, and the
/// unnamed result local took a continuation FIELD whenever the expansion crossed a suspension.
///
/// That field is the user-visible cost of the loop form and the reason this change is worth making.
/// Counting `While` nodes says the loop is gone; only this says what its absence buys.
///
/// Both compilers' complete arrays and field lists are stated, because they do not yet agree and
/// the remaining difference is not this change's: krusty names only `tag` where kotlinc also names
/// `tag$iv` and `p`, the inline expansion's own parameters. Giving those their own slots is the
/// spill-provenance work, not this; asserting kotlinc's numbers on both sides here would be
/// asserting that work instead.
///
/// What IS this change's is measured against the branch's own base: three reference fields there,
/// two here. The one that went is the expansion's result local — a slot the source never wrote,
/// live across the suspension for as long as the loop form allocated it before the body ran.
#[test]
fn a_tail_only_inline_expansion_keeps_its_value_on_the_stack() {
    let src = "suspend fun step(v: String): String = v\n\
               \n\
               inline fun <T> quick(tag: String, block: (String) -> T): T = block(tag + \"!\")\n\
               \n\
               suspend fun run(tag: String): String {\n\
               \x20   return quick(tag) { p -> step(p) + p }\n\
               }\n";
    let (reference, krusty) =
        disassemble_verbose("TailInlineExpansion", src, "TailInlineExpansionKt$run$1");
    assert_eq!(
        (
            metadata_array(&reference, "s"),
            metadata_array(&reference, "n")
        ),
        (
            vec!["L$0".to_string(), "L$1".to_string(), "L$2".to_string()],
            vec!["tag".to_string(), "tag$iv".to_string(), "p".to_string()],
        ),
        "kotlinc's complete spill arrays",
    );
    assert_eq!(
        (metadata_array(&krusty, "s"), metadata_array(&krusty, "n")),
        (vec!["L$0".to_string()], vec!["tag".to_string()]),
        "krusty's complete spill arrays: `tag$iv` and `p` are #1037's, not this change's",
    );

    // The field layout is the half the `@DebugMetadata` arrays cannot show, and the half this
    // change moves: the third reference field was the expansion's result local.
    let (reference, krusty) = disassemble_both(
        "TailInlineExpansionFields",
        src,
        "TailInlineExpansionFieldsKt$run$1",
    );
    assert_eq!(
        continuation_fields(&reference),
        ["L$0", "L$1", "L$2"],
        "kotlinc's complete field list",
    );
    assert_eq!(
        continuation_fields(&krusty),
        ["L$0", "L$1"],
        "krusty's complete field list, one shorter than this branch's base",
    );
}

/// The `n` array of a continuation's `@DebugMetadata`, asserted equal between the compilers, with
/// KRUSTY's `s` array returned alongside it. Use this where the names agree but the field positions
/// do not yet, so a test can pin the names and state the positions separately.
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

/// The `s` and `n` arrays of a continuation's `@DebugMetadata`, from BOTH compilers — asserted
/// equal, and returned so the test can also state what they are.
/// One `@DebugMetadata` array of a disassembled continuation class.
///
/// An absent attribute is a failure, not an empty array: a case that expects no spills at all would
/// otherwise pass against a class that carries no `@DebugMetadata` whatsoever, which is the one
/// thing these differentials exist to catch.
fn metadata_array(text: &str, key: &str) -> Vec<String> {
    let list = text
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix(key)?.strip_prefix("=["))
        .unwrap_or_else(|| panic!("no `{key}` array in the class's @DebugMetadata:\n{text}"));
    let list = list.trim_end_matches(']');
    if list.is_empty() {
        return Vec::new();
    }
    list.split(',')
        .map(|entry| entry.trim().trim_matches('"').to_string())
        .collect()
}

/// The spill fields of a disassembled continuation class, in layout order.
fn continuation_fields(text: &str) -> Vec<String> {
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
        .collect()
}

fn debug_metadata_spills(src: &str, name: &str, class: &str) -> (Vec<String>, Vec<String>) {
    let (reference, krusty) = disassemble_verbose(name, src, class);
    let array = |text: &str, key: &str| metadata_array(text, key);
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

/// The spill fields of one continuation class, in layout order, from BOTH compilers — asserted
/// equal, and returned so the test can also state what they are.
fn spill_fields(src: &str, name: &str, class: &str) -> Vec<String> {
    let (reference, krusty) = disassemble_both(name, src, class);
    let fields = continuation_fields;
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
