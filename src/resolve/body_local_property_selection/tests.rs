use crate::diag::{DiagSink, Severity, Span};
use crate::features::LangFeatures;
use crate::source::SourceInput;

fn checked_diagnostics(source: &str) -> Vec<(u32, Span, Severity, String)> {
    let inputs = [SourceInput::kotlin(source).with_file_stem("BodyLocalMemberExtension")];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        crate::toolchain::jvm_classpath_jars_for(source),
    ));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(
            crate::jvm::jvm_libraries::JvmLibraries::new(classpath)
                .expect("JVM provider initialization"),
        )),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    let _ = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    diagnostics
        .diags
        .into_iter()
        .map(|diagnostic| {
            (
                diagnostic.file,
                diagnostic.span,
                diagnostic.severity,
                diagnostic.msg,
            )
        })
        .collect()
}

#[test]
fn body_local_member_extension_val_stops_write_selection_before_farther_var() {
    let source = r#"var Int.token: Int
    get() = 0
    set(value) {}

fun build() {
    class Local {
        val Int.token: Int get() = 1
        fun write() {
            1.token = 1
        }
    }
}
"#;
    let start = u32::try_from(source.find("1.token = 1").expect("assignment target"))
        .expect("source offset fits u32");
    assert_eq!(
        checked_diagnostics(source),
        [(
            0,
            Span::new(start + 2, start + 7),
            Severity::Error,
            "'val' cannot be reassigned.".to_string(),
        )]
    );
}

#[test]
fn body_local_hidden_property_and_getter_fall_through_to_visible_extensions() {
    let source = r#"val Int.hiddenProperty: String get() = "outer property"
val Int.hiddenGetter: String get() = "outer getter"

fun build() {
    class Local {
        @Deprecated("gone", level = DeprecationLevel.HIDDEN)
        val Int.hiddenProperty: Int get() = 1

        val Int.hiddenGetter: Int
            @Deprecated("gone", level = DeprecationLevel.HIDDEN) get() = 1

        fun property(): String = 1.hiddenProperty
        fun getter(): String = 1.hiddenGetter
    }
}
"#;
    assert_eq!(checked_diagnostics(source), []);
}

#[test]
fn body_local_hidden_setter_falls_through_to_visible_extension_setter() {
    let source = r#"var Int.token: String
    get() = "outer"
    set(value) {}

fun build() {
    class Local {
        var Int.token: Int
            get() = 1
            @Deprecated("gone", level = DeprecationLevel.HIDDEN) set(value) {}

        fun write() {
            1.token = "OK"
        }
    }
}
"#;
    assert_eq!(checked_diagnostics(source), []);
}

#[test]
fn finalized_local_subclass_inherits_pass_two_property_surface() {
    let source = r#"fun box(): String {
    val captured = "K"
    open class Base(val first: String) {
        constructor() : this("O")
        val second: String
            get() = captured
    }
    class Derived : Base()
    val value = Derived()
    return value.first + value.second
}
"#;
    assert_eq!(checked_diagnostics(source), []);
}

#[test]
fn deferred_local_member_extension_header_is_not_reinferred_as_an_ordinary_property() {
    let source = r#"class ActualReceiver(val value: String)
typealias Receiver = ActualReceiver

interface Left {
    val Receiver.suffix: String
}

interface Right {
    val Receiver.suffix: String
}

interface Both : Left, Right

fun exercise(receiver: Receiver): String {
    class Local : Both {
        override val Receiver.suffix: String get() = this.value
    }
    return with(Local()) { receiver.suffix }
}
"#;
    assert_eq!(checked_diagnostics(source), []);
}
