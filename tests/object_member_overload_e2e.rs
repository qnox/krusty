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
        name: "plugin static fallback",
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
        outcome: Outcome::Clean,
    },
];

#[test]
fn singleton_member_calls_use_the_shared_source_set_harness() {
    for case in CASES {
        let source_texts = case
            .sources
            .iter()
            .map(|(_, source)| *source)
            .collect::<Vec<_>>();
        let diagnostics = common::front_end_diagnostics_files(&source_texts, &[], None);
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
