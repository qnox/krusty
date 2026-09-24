//! An UNRESOLVED member behind `?.` must be a frontend diagnostic, exactly as for the qualified
//! form. The safe-call checker arm for a call (`args: Some(..)`) exhausted every callable origin and
//! returned `Ty::Error` WITHOUT reporting anything; only the property form (`args: None`) reported,
//! because it routes through `check_member`. For a `String?` receiver the backend bail
//! ("this construct is not yet supported by the IR backend") then did frontend duty, and for a
//! statically-`null` receiver the lowerer's always-null fold returned before even that — so
//! `null?.thisDoesNotExistAnywhere()` compiled clean.
//!
//! kotlinc: `error: unresolved reference 'thisDoesNotExistAnywhere'`.
use super::common;

/// Run the front end with stdlib + JDK on the classpath, reading each message as a ledger header.
fn diags(src: &str) -> Vec<String> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    headers(common::front_end_diagnostics(
        src,
        &[stdlib],
        Some(jdk.as_path()),
    ))
}

/// Recorded kotlinc messages are ledger headers: kotlinc prints a candidate list on continuation
/// lines, which the ledger does not read. Read krusty's messages the same way.
fn headers(messages: Vec<String>) -> Vec<String> {
    messages
        .into_iter()
        .map(|message| message.lines().next().unwrap_or_default().to_string())
        .collect()
}

/// krusty reports the unresolved `name` behind `?.` exactly as kotlinc does (recorded per Kotlin
/// version: 2.4.20 names the non-null receiver type, `Nothing` for a `null` receiver).
fn assert_unresolved(src: &str, name: &str) {
    let d = diags(src);
    let expected = common::recorded(|| common::reference_error_messages("Main", src));
    assert!(
        !expected.is_empty(),
        "kotlinc must reject `{name}` in {src:?}"
    );
    assert_eq!(d, expected, "complete ordered diagnostics for {src:?}");
}

fn assert_accepted(src: &str) {
    let d = diags(src);
    assert!(
        d.is_empty(),
        "expected no diagnostics for {src:?}, got {d:?}"
    );
}

fn assert_argument_mismatch(label: &str, src: &str) {
    assert_rejected_as_kotlinc(label, src);
}

fn assert_inapplicable(label: &str, src: &str) {
    assert_rejected_as_kotlinc(label, src);
}

fn assert_rejected_as_kotlinc(label: &str, src: &str) {
    let expected = common::recorded_named(label, || common::reference_error_messages("Main", src));
    assert!(
        !expected.is_empty(),
        "kotlinc must reject the inapplicable call in {src:?}"
    );
    assert_eq!(
        diags(src),
        expected,
        "complete ordered diagnostics for {src:?}"
    );
}

/// A known divergence: kotlinc's recorded messages and krusty's, each exact.
fn assert_rejected_divergently(label: &str, src: &str, krusty: &[&str]) {
    let expected = common::recorded_named(label, || common::reference_error_messages("Main", src));
    assert!(
        !expected.is_empty(),
        "kotlinc must reject the inapplicable call in {src:?}"
    );
    assert_ne!(
        expected, krusty,
        "krusty now matches kotlinc for {src:?}: compare them with assert_inapplicable"
    );
    assert_eq!(
        diags(src),
        krusty,
        "krusty's complete ordered diagnostics for {src:?}"
    );
}

/// The reported shape: a statically-`null` receiver. The always-null fold must not buy the program
/// out of member resolution.
#[test]
fn null_receiver_unresolved_call_is_reported() {
    assert_unresolved(
        "fun box(): String {\n    val r = null?.thisDoesNotExistAnywhere()\n    return \"r=$r\"\n}\n",
        "thisDoesNotExistAnywhere",
    );
}

/// The same missing member on a `String?` receiver: previously only the BACKEND rejected it. The
/// diagnostic belongs in the checker, where the qualified form already reports it.
#[test]
fn nullable_string_receiver_unresolved_call_is_reported() {
    assert_unresolved(
        "fun f(s: String?): String? = s?.thisDoesNotExistAnywhere()\n",
        "thisDoesNotExistAnywhere",
    );
}

/// A user class receiver — the `Ty::Obj` arm of the same checker branch.
#[test]
fn class_receiver_unresolved_call_is_reported() {
    assert_unresolved(
        "class C\nfun f(c: C?): Any? = c?.thisDoesNotExistAnywhere()\n",
        "thisDoesNotExistAnywhere",
    );
}

/// A nullable primitive receiver — the non-`String`, non-`Obj` arm.
#[test]
fn nullable_primitive_receiver_unresolved_call_is_reported() {
    assert_unresolved(
        "fun f(i: Int?): Any? = i?.thisDoesNotExistAnywhere()\n",
        "thisDoesNotExistAnywhere",
    );
}

/// The property form already reported; lock it so the call-form fix keeps one message shape.
#[test]
fn unresolved_property_behind_safe_call_still_reported() {
    assert_unresolved(
        "fun box(): String {\n    val r = null?.thisDoesNotExist\n    return \"r=$r\"\n}\n",
        "thisDoesNotExist",
    );
}

// --- regression locks: resolvable safe calls must stay clean -----------------------------------

#[test]
fn resolvable_safe_calls_are_not_reported() {
    assert_accepted("fun f(s: String?): String? = s?.trim()\n");
    assert_accepted("fun f(s: String?): Int? = s?.length\n");
    assert_accepted("fun f(s: String?): String? = s?.let { it + \"!\" }\n");
    assert_accepted("fun f(i: Int?): String? = i?.toString()\n");
    assert_accepted(
        "fun f(s: String?): String? = s?.replace(oldValue = \"a\", newValue = \"b\")\n",
    );
    assert_accepted("class C { fun m(): Int = 1 }\nfun f(c: C?): Int? = c?.m()\n");
    assert_accepted("class C\nfun C.ext(): Int = 1\nfun f(c: C?): Int? = c?.ext()\n");
    assert_accepted("fun f(): String? = null?.toString()\n");
}

/// The report must fire only on a member that does NOT EXIST — never on one the safe-call arm merely
/// cannot select. Every line here is valid Kotlin that krusty still rejects in the BACKEND ("this
/// construct is not yet supported by the IR backend"); mislabelling those "unresolved reference"
/// would turn a krusty gap into a claim that the user's program is wrong, and these go to the LSP.
#[test]
fn unselectable_but_existing_members_are_not_called_unresolved() {
    assert_accepted("fun f(x: Boolean?): Boolean? = x?.not()\n");
    assert_accepted("fun f(x: Byte?): Int? = x?.toInt()\n");
    assert_accepted("fun f(x: Short?): Int? = x?.toInt()\n");
    assert_accepted("fun f(x: Double?): Int? = x?.toInt()\n");
    assert_accepted("fun f(x: Double?): Long? = x?.toLong()\n");
    assert_accepted("fun f(x: Long?): Int? = x?.toInt()\n");
    assert_accepted("fun f(x: Int?): Boolean? = x?.equals(1)\n");
    assert_accepted("fun f(x: UInt?): UInt? = x?.plus(1u)\n");
    assert_accepted("fun f(g: ((Int) -> Int)?): Int? = g?.invoke(1)\n");
    // Existing-but-inapplicable members get overload diagnostics, never "unresolved reference".
    assert_argument_mismatch("let-argument", "fun f(s: String?): Any? = s?.let(1)\n");
    assert_inapplicable(
        "substring-arity",
        "fun f(s: String?): Any? = s?.substring(9, 9, 9)\n",
    );
    // `Int.toString(radix)` is a real stdlib extension and is therefore applicable.
    assert_accepted("fun f(i: Int?): Any? = i?.toString(1)\n");
    // kotlinc 2.4.20 joins the rejected member with the same-name extensions it climbed past
    // (`Any?.hashCode()`); earlier versions report the member's own arity error.
    assert_inapplicable("hash-code-arity", "fun f(i: Int?): Any? = i?.hashCode(1)\n");
    // Both compilers reject `equals()`, but kotlinc reports it against the mapped Kotlin member
    // `equals(other: Any?)` while krusty still sees the Java `Object.equals` overloads
    // (docs/IMPLEMENTATION_PLAN.md). krusty's exact output is pinned beside kotlinc's recorded one,
    // so neither can drift unnoticed.
    assert_rejected_divergently(
        "equals-arity",
        "fun f(i: Int?): Any? = i?.equals()\n",
        &["none of the following candidates is applicable:"],
    );
}

/// The classpath-less `String` table stands in for stdlib EXTENSIONS (`kotlin.String` has no
/// `concat`/`substring`/`indexOf` member), so a user's own extension of the same name must out-rank
/// it. Run with NO classpath — the only mode in which the table is consulted at all.
#[test]
fn user_string_extension_outranks_the_classpath_less_table() {
    let d = common::front_end_diagnostics(
        "fun String.concat(o: String): Int = 42\n\
         fun f(s: String?): Int? = s?.concat(\"x\")\n",
        &[],
        None,
    );
    assert!(
        d.is_empty(),
        "the source extension must win over the builtin `concat` table, got {d:?}"
    );
}

/// The classpath-less builtin declaration is still an ordinary candidate: an invalid call must
/// publish the same complete applicability diagnostic as the metadata-backed declaration, not be
/// accepted or mislabeled as an unresolved name.
#[test]
fn classpath_less_string_overload_mismatch_is_not_called_unresolved() {
    const SOURCE: &str = "fun f(s: String?): Any? = s?.substring(9, 9, 9)\n";
    let expected = common::recorded(|| common::reference_error_messages("Main", SOURCE));
    assert!(!expected.is_empty(), "kotlinc must reject the invalid call");
    assert_eq!(
        headers(common::front_end_diagnostics(SOURCE, &[], None)),
        expected,
        "the classpath-less declaration must preserve exact applicability diagnostics"
    );
}

/// A `Nothing?` receiver: its only value is `null`, so the call is never made and the result is
/// `null`. Member resolution has no class to look a member up on — the always-null rule applies,
/// but ONLY for a member that a `Nothing?` receiver could plausibly carry (`Any` methods), so the
/// acceptance hole closed above is not reopened.
#[test]
fn nothing_nullable_receiver_calls_any_member() {
    const SRC: &str = "fun box(): String {\n\
            val n: Nothing? = null\n\
            val r = n?.toString()\n\
            return if (r == null) \"OK\" else \"F:$r\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "nothing_nullable_safe_call");
}

#[test]
fn nothing_nullable_receiver_unresolved_call_is_reported() {
    assert_unresolved(
        "fun f(): Any? {\n    val n: Nothing? = null\n    return n?.thisDoesNotExistAnywhere()\n}\n",
        "thisDoesNotExistAnywhere",
    );
}
