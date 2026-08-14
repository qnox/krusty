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

/// Run the front end with stdlib + JDK on the classpath.
fn diags(src: &str) -> Vec<String> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    common::front_end_diagnostics(src, &[stdlib], Some(jdk.as_path()))
}

fn assert_unresolved(src: &str, name: &str) {
    let d = diags(src);
    assert!(
        d.iter()
            .any(|m| m.contains(&format!("unresolved reference '{name}'."))),
        "expected `unresolved reference '{name}'.` for {src:?}, got {d:?}"
    );
}

fn assert_accepted(src: &str) {
    let d = diags(src);
    assert!(
        d.is_empty(),
        "expected no diagnostics for {src:?}, got {d:?}"
    );
}

fn assert_argument_mismatch(src: &str) {
    let d = diags(src);
    assert!(
        d.iter()
            .any(|message| message.contains("argument type mismatch")),
        "expected an argument mismatch for {src:?}, got {d:?}"
    );
}

fn assert_inapplicable(src: &str) {
    let d = diags(src);
    assert!(
        d.iter().any(|message| {
            message.contains("argument type mismatch")
                || message.starts_with("none of the following candidates is applicable:")
                || message.starts_with("too many arguments for")
                || message.starts_with("function '")
        }),
        "expected an inapplicable-call diagnostic for {src:?}, got {d:?}"
    );
    assert!(
        d.iter()
            .all(|message| !message.contains("unresolved reference")),
        "an existing member must not be called unresolved: {d:?}"
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
    assert_argument_mismatch("fun f(s: String?): Any? = s?.let(1)\n");
    assert_inapplicable("fun f(s: String?): Any? = s?.substring(9, 9, 9)\n");
    // `Int.toString(radix)` is a real stdlib extension and is therefore applicable.
    assert_accepted("fun f(i: Int?): Any? = i?.toString(1)\n");
    assert_inapplicable("fun f(i: Int?): Any? = i?.hashCode(1)\n");
    assert_inapplicable("fun f(i: Int?): Any? = i?.equals()\n");
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

/// The no-classpath fallback's NAME still exists when this particular invocation cannot select one
/// of its recorded shapes. Existence and applicability are separate questions: this compiler may
/// lack the overload diagnostic, but it must not claim that `substring` itself is unresolved.
#[test]
fn classpath_less_string_overload_mismatch_is_not_called_unresolved() {
    let diagnostics = common::front_end_diagnostics(
        "fun f(s: String?): Any? = s?.substring(9, 9, 9)\n",
        &[],
        None,
    );
    assert!(
        diagnostics
            .iter()
            .all(|message| !message.contains("unresolved reference 'substring'.")),
        "an existing fallback name must not be reported unresolved: {diagnostics:?}"
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
