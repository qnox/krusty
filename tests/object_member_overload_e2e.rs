use super::common;

enum Outcome {
    Clean,
    Error(&'static str),
    BoxOk,
}

struct Case {
    name: &'static str,
    sources: &'static [(&'static str, &'static str)],
    outcome: Outcome,
}

const CASES: &[Case] = &[
    Case {
        name: "member-extension collision",
        sources: &[(
            "Main",
            r#"
class Scope

abstract class Base {
    fun choose(a: Int, b: Int, c: Int, d: Int) = a + b + c + d
    protected abstract fun Scope.choose(a: String, b: String): String
}

object Derived : Base() {
    override fun Scope.choose(a: String, b: String) = a + b
}

fun result() = Derived.choose(a = 1, b = 2, c = 3, d = 4)
"#,
        )],
        outcome: Outcome::Clean,
    },
    Case {
        name: "same-file exact target and named slots",
        sources: &[(
            "Main",
            r#"
abstract class Base {
    fun choose(a: Int, b: Int, c: Int, d: Int) =
        a * 1000 + b * 100 + c * 10 + d
}

object Derived : Base() {
    fun choose(a: String, b: String) = a + b
}

fun box() =
    if (Derived.choose(d = 4, b = 2, a = 1, c = 3) == 1234) "OK" else "fail"
"#,
        )],
        outcome: Outcome::BoxOk,
    },
    Case {
        name: "cross-file inherited target with defaults",
        sources: &[
            (
                "Types",
                r#"
abstract class Base {
    fun choose(a: Int = 1, b: Int = 2, c: Int = 3, d: Int = 4) =
        a * 1000 + b * 100 + c * 10 + d
}

object Derived : Base() {
    fun choose(a: String, b: String) = a + b
}
"#,
            ),
            (
                "Main",
                r#"
fun box() =
    if (Derived.choose(d = 4, b = 2) == 1234) "OK" else "fail"
"#,
            ),
        ],
        outcome: Outcome::BoxOk,
    },
    Case {
        name: "local object with sibling owner",
        sources: &[
            (
                "Types",
                r#"
abstract class Base {
    fun choose(a: Int = 1, b: Int = 2) = a * 10 + b
}
"#,
            ),
            (
                "Main",
                r#"
object Derived : Base()

fun box() = if (Derived.choose(b = 2) == 12) "OK" else "fail"
"#,
            ),
        ],
        outcome: Outcome::BoxOk,
    },
    Case {
        name: "same-file inherited defaults",
        sources: &[(
            "Main",
            r#"
abstract class Base {
    fun choose(a: Int = 1, b: Int = 2, c: Int = 3, d: Int = 4) =
        a * 1000 + b * 100 + c * 10 + d
}

object Derived : Base()

fun box() =
    if (Derived.choose(d = 4, b = 2) == 1234) "OK" else "fail"
"#,
        )],
        outcome: Outcome::BoxOk,
    },
    Case {
        name: "same-file inherited vararg",
        sources: &[(
            "Main",
            r#"
abstract class Base {
    fun choose(vararg values: Int) = values[0] + values[1] + values[2]
}

object Derived : Base() {
    fun choose(a: String, b: String) = a + b
}

fun box() = if (Derived.choose(1, 2, 3) == 6) "OK" else "fail"
"#,
        )],
        outcome: Outcome::BoxOk,
    },
    Case {
        name: "positional vararg overload",
        sources: &[(
            "Main",
            r#"
abstract class Base {
    fun choose(prefix: String, vararg values: Int) = 0
    fun choose(prefix: Int, vararg values: Int) = prefix + values[0] + values[1]
}

object Derived : Base()

fun box() = if (Derived.choose(1, 2, 3) == 6) "OK" else "fail"
"#,
        )],
        outcome: Outcome::BoxOk,
    },
    Case {
        name: "cross-file inherited vararg",
        sources: &[
            (
                "Types",
                r#"
abstract class Base {
    fun choose(vararg values: Int) = values[0] + values[1] + values[2]
}

object Derived : Base()
"#,
            ),
            (
                "Main",
                r#"
fun box() = if (Derived.choose(1, 2, 3) == 6) "OK" else "fail"
"#,
            ),
        ],
        outcome: Outcome::BoxOk,
    },
    Case {
        name: "cross-file erased generic return",
        sources: &[
            (
                "Types",
                r#"
abstract class Base {
    fun <T> identity(value: T): T = value
}

object Derived : Base()
"#,
            ),
            (
                "Main",
                r#"
fun box() = if (Derived.identity("OK").length == 2) "OK" else "fail"
"#,
            ),
        ],
        outcome: Outcome::BoxOk,
    },
    Case {
        name: "cross-file value class return",
        sources: &[
            (
                "Types",
                r#"
@JvmInline
value class Wrapped(val value: Any)

interface Operation<T> {
    fun performOperation(): T
}

object ResultOperation : Operation<Wrapped> {
    override fun performOperation(): Wrapped = Wrapped(1)
}
"#,
            ),
            (
                "Main",
                r#"
fun box(): String {
    val result = ResultOperation.performOperation()
    return if ("$result" == "Wrapped(value=1)") "OK" else "$result"
}
"#,
            ),
        ],
        outcome: Outcome::BoxOk,
    },
    Case {
        name: "public overload visibility",
        sources: &[(
            "Main",
            r#"
object Visible {
    private fun choose(value: Int) = value
    fun choose(value: String) = value
}

fun result() = Visible.choose("OK")
"#,
        )],
        outcome: Outcome::Clean,
    },
    Case {
        name: "selected overload visibility",
        sources: &[(
            "Main",
            r#"
object Hidden {
    private fun choose(value: Int) = value
}

fun result() = Hidden.choose(1)
"#,
        )],
        outcome: Outcome::Error("cannot access 'choose': it is private"),
    },
    Case {
        name: "inherited protected visibility",
        sources: &[(
            "Main",
            r#"
abstract class Base {
    protected fun choose(value: Int) = value
}

object Derived : Base()

fun result() = Derived.choose(1)
"#,
        )],
        outcome: Outcome::Error("cannot access 'choose': it is protected"),
    },
    Case {
        name: "an annotation does not synthesize an inactive plugin callable",
        sources: &[(
            "Main",
            r#"
@Serializable
object Token {
    fun serializer(value: Int) = value
}

fun result() = Token.serializer()
"#,
        )],
        outcome: Outcome::Error("none of the following candidates is applicable"),
    },
];

#[test]
fn singleton_member_calls_use_the_shared_source_set_harness() {
    let stdlib = common::stdlib_jar();
    for case in CASES {
        let source_texts = case
            .sources
            .iter()
            .map(|(_, source)| *source)
            .collect::<Vec<_>>();
        let diagnostics =
            common::front_end_diagnostics_files(&source_texts, std::slice::from_ref(&stdlib), None);
        match case.outcome {
            Outcome::Clean | Outcome::BoxOk => assert!(
                diagnostics.is_empty(),
                "{}: unexpected diagnostics: {diagnostics:?}",
                case.name
            ),
            Outcome::Error(expected) => {
                assert!(
                    diagnostics.iter().any(|message| message.contains(expected)),
                    "{}: expected {expected:?}, got {diagnostics:?}",
                    case.name
                );
                continue;
            }
        }
        if matches!(case.outcome, Outcome::BoxOk) {
            assert_eq!(
                common::compile_and_run_files_with_stdlib(case.sources).as_deref(),
                Some("OK"),
                "{}",
                case.name
            );
        }
    }
}
