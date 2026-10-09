use super::common;

fn abstract_writer_classpath() -> std::path::PathBuf {
    let source = "package fixture; public abstract class AbstractWriter { \
                  public abstract void write(int value); }";
    common::javac_compile(
        &[(
            "fixture/AbstractWriter.java".to_string(),
            source.to_string(),
        )],
        &[],
    )
    .expect("javac compiles the repository-owned abstract base")
    .0
}

fn expect_box_with_classpath(source: &str, stem: &str, dependency: &std::path::Path) {
    let classpath = [dependency.to_path_buf()];
    let reference = common::e2e::kotlinc_box_result_with_classpath(source, &classpath);
    assert_eq!(reference, "OK", "{stem}: kotlinc fixture must succeed");
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    assert_eq!(
        common::expect_box_run(
            source,
            stem,
            &[dependency.to_path_buf(), stdlib],
            Some(jdk.as_path()),
        ),
        reference,
        "{stem}: krusty and kotlinc box results differ",
    );
}

#[test]
fn named_and_anonymous_classes_extend_an_abstract_classpath_class() {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let Some(libout) = common::compile_lib(
        "absbase",
        "package lib\n\
         abstract class Greeter {\n\
         \x20 fun greet(name: String): String = \"hi \" + name\n\
         }\n",
    ) else {
        return;
    };
    let cp = vec![libout.clone(), sl.clone()];
    let main = "import lib.Greeter\n\
        class NamedGreeter : Greeter()\n\
        fun box(): String {\n\
        \x20 val anonymous: Greeter = object : Greeter() {}\n\
        \x20 val named: Greeter = NamedGreeter()\n\
        \x20 return if (anonymous.greet(\"anonymous\") == \"hi anonymous\" && named.greet(\"named\") == \"hi named\") \"OK\" else \"fail\"\n\
        }\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(jdk.as_path()))
        .expect("krusty failed to subclass an abstract classpath class");
    match common::run_box(&classes, "MainKt", &[libout, sl]) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}

#[test]
fn unsafe_abstract_classpath_bases_are_declined() {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let Some(libout) = common::compile_lib_ref(
        "unsafeabsbase",
        "package lib\n\
         abstract class RequiresOverride { abstract fun value(): String }\n\
         abstract class Closed private constructor()\n",
    ) else {
        return;
    };
    let cp = vec![libout, sl];

    // A subclass that DISCHARGES every abstract obligation now compiles (the gate checks the
    // obligation list against the class's overrides); one that leaves an obligation open still
    // declines.
    let implements_abstract =
        "import lib.RequiresOverride\nclass Child : RequiresOverride() { override fun value() = \"x\" }\n";
    assert!(
        common::compile_in_process(implements_abstract, "Override", &cp, Some(jdk.as_path()))
            .is_some(),
        "discharging every abstract obligation must subclass"
    );
    let leaves_abstract_open = "import lib.RequiresOverride\nclass Child : RequiresOverride()\n";
    assert!(common::compile_in_process(
        leaves_abstract_open,
        "OverrideOpen",
        &cp,
        Some(jdk.as_path())
    )
    .is_none());

    let inaccessible_constructor = "import lib.Closed\nclass Child : Closed()\n";
    assert!(common::compile_in_process(
        inaccessible_constructor,
        "Closed",
        &cp,
        Some(jdk.as_path())
    )
    .is_none());
}

/// A sealed class may leave an inherited classpath obligation for its concrete child.
#[test]
fn sealed_class_may_leave_an_inherited_abstract_member() {
    let dependency = abstract_writer_classpath();
    let source = r#"
import fixture.AbstractWriter

sealed class Sink : AbstractWriter()

class RecordingSink : Sink() {
    var written: Int = -1
    override fun write(b: Int) {
        written = b
    }
}

fun box(): String {
    val sink = RecordingSink()
    sink.write(7)
    return if (sink.written == 7) "OK" else "fail"
}
"#;
    expect_box_with_classpath(source, "sealed-inherited-abstract", &dependency);
}

/// A sealed class may also declare its own abstract member for a concrete child to implement.
#[test]
fn sealed_class_may_declare_its_own_abstract_member() {
    let source = r#"
sealed class Source {
    abstract fun value(): String
}

class ValueSource : Source() {
    override fun value(): String = "OK"
}

fun box(): String = ValueSource().value()
"#;
    common::e2e::expect_box_same_as_kotlinc(source, "sealed-own-abstract");
}

fn assert_diagnostics(
    name: &str,
    source: &str,
    classpath: &[std::path::PathBuf],
    krusty: common::e2e::CompilerError,
    reference: common::e2e::CompilerError,
) {
    let result = common::e2e::compiler_diagnostics(&[(name, source)], classpath);
    assert_eq!(result.krusty_code, 1, "{name}: {}", result.krusty_stderr);
    assert_eq!(
        result.reference_code, 1,
        "{name}: {}",
        result.reference_stderr
    );
    assert_eq!(result.krusty_stdout, "", "{name}");
    assert_eq!(
        common::e2e::compiler_errors(&result.krusty_stderr),
        [krusty]
    );
    assert_eq!(
        common::e2e::compiler_errors(&result.reference_stderr),
        [reference]
    );
}

/// Neither a final nor an open class may leave an inherited or source-declared abstract member.
/// Pin both compiler streams completely: their current wording differs, but both exact locations,
/// messages, counts and order are part of the regression contract.
#[test]
fn concrete_and_open_classes_must_discharge_both_abstract_obligation_sources() {
    use common::e2e::CompilerError;

    let dependency = abstract_writer_classpath();
    for (name, prefix, column) in [
        ("InheritedFinal.kt", "", 1),
        ("InheritedOpen.kt", "open ", 6),
    ] {
        let source =
            format!("import fixture.AbstractWriter\n{prefix}class Bad : AbstractWriter()\n");
        assert_diagnostics(
            name,
            &source,
            std::slice::from_ref(&dependency),
            CompilerError {
                file: name.to_string(),
                line: 2,
                column,
                message: "class 'Bad' is not abstract and does not implement all abstract members"
                    .to_string(),
            },
            CompilerError {
                file: name.to_string(),
                line: 2,
                column,
                message: "class 'Bad' is not abstract and does not implement abstract base class member:\nfun write(p0: Int): Unit".to_string(),
            },
        );
    }

    for (name, prefix, krusty_column, reference_column) in
        [("OwnFinal.kt", "", 22, 13), ("OwnOpen.kt", "open ", 27, 18)]
    {
        let source = format!("{prefix}class Bad {{ abstract fun value(): String }}\n");
        assert_diagnostics(
            name,
            &source,
            &[],
            CompilerError {
                file: name.to_string(),
                line: 1,
                column: krusty_column,
                message: "abstract member 'value' is not allowed in a non-abstract class"
                    .to_string(),
            },
            CompilerError {
                file: name.to_string(),
                line: 1,
                column: reference_column,
                message: "abstract function 'value' in non-abstract class 'Bad'.".to_string(),
            },
        );
    }
}
