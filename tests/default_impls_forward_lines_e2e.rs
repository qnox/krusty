//! A `$DefaultImpls` compatibility forward spans the declaration it stands for, as kotlinc's does:
//! the call is on the declaration's start line (the `fun` line, even when an expression body
//! starts later) and the return on the line the declaration ends on (a block's `}`, an expression
//! body's last line, an accessor body's end).

use super::common;

const SOURCE: &str = "\
package store\n\
\n\
fun mark(v: Int) = v\n\
\n\
interface Forwarded {\n\
    fun block(): String {\n\
        return \"b\"\n\
    }\n\
\n\
    fun expression(): String =\n\
        \"x\"\n\
\n\
    fun wrapped(): String = \"y\" +\n\
        \"z\"\n\
\n\
    fun unit(a: Int) {\n\
        mark(a)\n\
    }\n\
\n\
    fun single() = 1\n\
\n\
    val p: String\n\
        get() =\n\
            \"a\"\n\
\n\
    var q: Int\n\
        get() = 1\n\
        set(v) {\n\
            mark(v)\n\
        }\n\
}\n\
";

const SUSPEND_SOURCE: &str = "\
package store\n\
\n\
interface SuspendForwarded {\n\
    suspend fun forwarded(): String {\n\
        return \"ok\"\n\
    }\n\
}\n\
";

#[test]
fn default_impls_forwards_return_on_the_declaration_end_line() {
    for class in ["store/Forwarded$DefaultImpls", "store/Forwarded"] {
        common::byte_diff_against_kotlinc_cp(
            "DefaultImplsForwardLines",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}

const PARENTHESIZED_SOURCE: &str = "\
package store\n\
\n\
interface Parenthesized {\n\
    fun grouped(p: String): Boolean =\n\
        (\n\
            p == \"kotlin\" ||\n\
                p.startsWith(\"kotlin.\")\n\
        )\n\
\n\
    fun negated(p: String): Boolean =\n\
        !(\n\
            p == \"kotlin\"\n\
        )\n\
\n\
    fun operand(p: String): Int =\n\
        p.length +\n\
            (\n\
                p.length\n\
            )\n\
\n\
    fun closedBelow(p: String): Boolean = !(p.isEmpty()\n\
    )\n\
}\n\
";

/// An expression body's closing `)` belongs to the body, so the forward returns on its line.
#[test]
fn default_impls_forward_returns_on_the_closing_parenthesis_line() {
    common::byte_diff_against_kotlinc_cp(
        "DefaultImplsForwardParenthesized",
        PARENTHESIZED_SOURCE,
        "store/Parenthesized$DefaultImpls",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("store/Parenthesized$DefaultImpls differs from kotlinc: {diff}"));
}

#[test]
fn suspend_default_impls_forward_keeps_the_declaration_end_line() {
    let comparison = common::compare_with_kotlinc_plugin(
        "SuspendDefaultImplsForwardLines",
        SUSPEND_SOURCE,
        "store/SuspendForwarded$DefaultImpls",
        &[common::stdlib_jar()],
        "17",
        &[],
    )
    .expect("reference kotlinc and javap are provisioned");
    let header = comparison
        .reference
        .lines()
        .map(str::trim)
        .find(|line| {
            line.ends_with(
                " forwarded(store.SuspendForwarded, kotlin.coroutines.Continuation<? super java.lang.String>);",
            )
        })
        .expect("kotlinc emits the suspend DefaultImpls forward");
    let reference = common::method_block(&comparison.reference, header);
    let krusty = common::method_block(&comparison.krusty, header);
    assert_eq!(
        reference
            .iter()
            .filter(|line| line.starts_with("line "))
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["line 4: 0", "line 6: 5"],
        "kotlinc's call/return line contract changed"
    );
    assert_eq!(krusty, reference, "suspend DefaultImpls forward differs");
}

#[test]
fn default_impls_forward_copies_the_member_and_parameter_annotations() {
    // kotlinc's forward carries the member's RUNTIME annotations before its own
    // `@java.lang.Deprecated`, the BINARY ones before `@NotNull`, and each parameter's annotations.
    const ANNOTATED: &str = "\
package store\n\
\n\
annotation class Marker\n\
@Retention(AnnotationRetention.BINARY) annotation class Bin\n\
@Target(AnnotationTarget.VALUE_PARAMETER) annotation class P\n\
\n\
interface Annotated {\n\
    @Marker @Bin\n\
    fun close(@P x: String?): String {\n\
        return \"\"\n\
    }\n\
\n\
    @Deprecated(\"old\")\n\
    fun old() {}\n\
}\n\
";
    common::byte_diff_against_kotlinc_cp(
        "DefaultImplsForwardAnnotations",
        ANNOTATED,
        "store/Annotated$DefaultImpls",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("store/Annotated$DefaultImpls differs from kotlinc: {diff}"));
}
