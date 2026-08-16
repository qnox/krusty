//! An expected result fixes a call's type arguments when the declared return type mentions them
//! invariantly. `fun <T> reply(body: T): Reply<T>` called where `Reply<Any>` is expected must bind
//! `T = Any`: `Reply<String>` is not a `Reply<Any>`, so the argument-derived lower bound has to
//! widen to the only solution the expected type admits. Covariant positions keep the narrow
//! solution, which is why `listOf("x")` in a `List<Any>` position still infers `List<String>`.

use super::common;

#[test]
fn expected_result_fixes_invariant_type_arguments() {
    const SOURCE: &str = r#"
        interface Reply<B> {
            fun payload(): B
        }

        interface MutableReply<B> : Reply<B>

        class Cell<B>(private val value: B) : MutableReply<B> {
            override fun payload(): B = value
        }

        fun <T> reply(body: T): Reply<T> = Cell(body)

        fun <T> mutableReply(body: T): MutableReply<T> = Cell(body)

        fun sameOwner(): Reply<Any> = reply("same")

        fun subOwner(): Reply<Any> = mutableReply("sub")

        fun branches(flag: Boolean): Reply<Any> = if (flag) reply("then") else mutableReply(2)

        fun covariant(): List<Any> = listOf("covariant")

        fun box(): String {
            if (sameOwner().payload() != "same") return "sameOwner"
            if (subOwner().payload() != "sub") return "subOwner"
            if (branches(true).payload() != "then") return "then"
            if (branches(false).payload() != 2) return "else"
            if (covariant().first() != "covariant") return "covariant"
            return "OK"
        }
    "#;
    let (code, diagnostics) = common::kotlinc_source_result("InvariantExpectedResult", SOURCE);
    assert_eq!(
        code, 0,
        "kotlinc rejected the control source: {diagnostics}"
    );
    let krusty = common::front_end_diagnostics_with_stdlib(SOURCE);
    assert!(
        krusty.is_empty(),
        "an expected invariant result must fix the call's type arguments: {krusty:?}"
    );
    let jdk = common::jdk_modules();
    let classpath = [common::stdlib_jar()];
    let Some(output) =
        common::compile_and_run_box(SOURCE, "InvariantExpectedResult", &classpath, Some(&jdk))
    else {
        panic!("expected the invariant-result fixture to compile and run");
    };
    assert_eq!(output.trim(), "OK");
}

#[test]
fn an_unsatisfiable_expected_result_still_reports_the_mismatch() {
    // Widening is only sound when the argument's own type still fits the expected binding. `Int` is
    // not a `String`, so the call stays rejected instead of silently adopting the expected type.
    const SOURCE: &str = r#"
        interface Reply<B>

        class Cell<B>(private val value: B) : Reply<B>

        fun <T> reply(body: T): Reply<T> = Cell(body)

        fun mismatch(): Reply<String> = reply(2)
    "#;
    let (code, _) = common::kotlinc_source_result("UnsatisfiableExpectedResult", SOURCE);
    assert_ne!(code, 0, "kotlinc must reject Int bound to Reply<String>");
    let krusty = common::front_end_diagnostics_with_stdlib(SOURCE);
    assert!(
        krusty
            .iter()
            .any(|diagnostic| diagnostic.contains("type mismatch")),
        "an unsatisfiable expected result must remain a checked error: {krusty:?}"
    );
}

/// The shape that motivated this: a Java response hierarchy whose static factories return a mutable
/// subtype (`static <T> MutableReply<T> ok(T body)`), used from a function declared to return the
/// read-only supertype `Reply<Any>`. Both the owner walk (`MutableReply<T>` seen as `Reply<T>`) and
/// the invariant widening (`T = Any`, not `T = String`) are required, including for the merged
/// branches of an `if`/`try` used as the function body and for `return` statements in a block body.
/// The fixtures are real implementations, so the compiled classes also run: a binding that widens
/// the erasure incorrectly would fail on the JVM rather than only in the checker.
#[test]
fn expected_result_fixes_type_arguments_of_a_java_static_factory() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let java = [
        (
            "Message.java".into(),
            r#"
                package fixtures;
                public interface Message<B> {
                    B getBody();
                }
            "#
            .into(),
        ),
        (
            "MutableMessage.java".into(),
            r#"
                package fixtures;
                public interface MutableMessage<B> extends Message<B> {
                    <T> MutableMessage<T> body(T body);
                }
            "#
            .into(),
        ),
        (
            "Reply.java".into(),
            r#"
                package fixtures;
                public interface Reply<B> extends Message<B> {
                    int getCode();
                    static <T> MutableReply<T> ok(T body) { return new ReplyImpl<T>(200, body); }
                    static <T> MutableReply<T> created(T body) { return new ReplyImpl<T>(201, body); }
                    static <T> MutableReply<T> badRequest(T body) { return new ReplyImpl<T>(400, body); }
                    static <T> MutableReply<T> noContent() { return new ReplyImpl<T>(204, null); }
                    static <T> MutableReply<T> status(int code) { return new ReplyImpl<T>(code, null); }
                }
            "#
            .into(),
        ),
        (
            "MutableReply.java".into(),
            r#"
                package fixtures;
                public interface MutableReply<B> extends Reply<B>, MutableMessage<B> {
                    <T> MutableReply<T> body(T body);
                }
            "#
            .into(),
        ),
        (
            "ReplyImpl.java".into(),
            r#"
                package fixtures;
                public final class ReplyImpl<B> implements MutableReply<B> {
                    private final int code;
                    private final B body;
                    public ReplyImpl(int code, B body) { this.code = code; this.body = body; }
                    public B getBody() { return body; }
                    public int getCode() { return code; }
                    public <T> MutableReply<T> body(T body) { return new ReplyImpl<T>(code, body); }
                }
            "#
            .into(),
        ),
    ];
    let Some((library, _)) = common::javac_compile(&java, &[]) else {
        return;
    };
    let root = library.parent().map(std::path::Path::to_path_buf);
    const SOURCE: &str = r#"
        import fixtures.Reply

        class Payload(val id: String)

        fun direct(): Reply<Any> = Reply.ok("body")

        fun chained(): Reply<Any> = Reply.status<Any>(409).body(mapOf("error" to "conflict"))

        fun empty(): Reply<Any> = Reply.noContent()

        fun branches(flag: Boolean): Reply<Any> =
            if (flag) Reply.ok(Payload("a")) else Reply.badRequest(mapOf("error" to "bad"))

        fun caught(fail: Boolean): Reply<Any> =
            try {
                if (fail) throw IllegalStateException("nope")
                Reply.created(Payload("a"))
            } catch (e: IllegalStateException) {
                Reply.status<Any>(403).body(mapOf("error" to "forbidden"))
            }

        fun listed(ok: Boolean, items: List<String>): Reply<List<String>> {
            if (!ok) {
                return Reply.noContent()
            }
            return Reply.ok(items)
        }

        fun box(): String {
            if (direct().body != "body") return "direct"
            if (direct().code != 200) return "direct code"
            if (chained().code != 409) return "chained code"
            if ((chained().body as Map<String, String>)["error"] != "conflict") return "chained body"
            if (empty().body != null) return "empty"
            if ((branches(true).body as Payload).id != "a") return "then"
            if (branches(false).code != 400) return "else"
            if (caught(false).code != 201) return "caught try"
            if (caught(true).code != 403) return "caught catch"
            if (listed(true, listOf("x")).body?.first() != "x") return "listed"
            if (listed(false, listOf("x")).code != 204) return "listed empty"
            return "OK"
        }
    "#;
    let (code, diagnostics) = common::kotlinc_source_result_with_args(
        "JavaStaticFactoryExpectedResult",
        SOURCE,
        &[
            "-cp".to_string(),
            library.to_string_lossy().into_owned(),
            "-nowarn".to_string(),
        ],
    );
    assert_eq!(
        code, 0,
        "kotlinc rejected the control source: {diagnostics}"
    );
    let classpath = [library, stdlib];
    let krusty = common::front_end_diagnostics(SOURCE, &classpath, Some(jdk.as_path()));
    assert!(
        krusty.is_empty(),
        "a Java static factory must bind its type argument from the expected result: {krusty:?}"
    );
    let output = common::compile_and_run_box(
        SOURCE,
        "JavaStaticFactoryExpectedResult",
        &classpath,
        Some(jdk.as_path()),
    );
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(
        output.as_deref().map(str::trim),
        Some("OK"),
        "the compiled classes must run against the Java fixtures"
    );
}
