//! A Java `?` argument for an F-bounded self-type parameter (`AbstractThrowableAssert<?, ? extends
//! Throwable>`, AssertJ's fluent API) is bounded by its declaration, so a member returning `SELF`
//! still exposes the class's own members, as in kotlinc.

use std::path::PathBuf;

use super::common;

const BASE: &str = "public abstract class Base<SELF extends Base<SELF, ACTUAL>, ACTUAL> {\n\
    \x20   public SELF isInstanceOf(Class<?> type) { return null; }\n\
    }\n";

const MID: &str = "public abstract class Mid<SELF extends Mid<SELF, ACTUAL>, ACTUAL> extends Base<SELF, ACTUAL> {}\n";

const THROWABLE: &str = "public class ThrowableAssert<SELF extends ThrowableAssert<SELF, ACTUAL>, ACTUAL extends Throwable>\n\
    \x20   extends Mid<SELF, ACTUAL> {\n\
    \x20   public SELF hasMessage(String message) { return null; }\n\
    \x20   public static ThrowableAssert<?, ? extends Throwable> assertThatThrownBy() { return null; }\n\
    }\n";

const USE: &str = "fun inherited() {\n\
    ThrowableAssert.assertThatThrownBy().isInstanceOf(String::class.java).hasMessage(\"m\")\n\
    }\n\
    fun declared() {\n\
    ThrowableAssert.assertThatThrownBy().hasMessage(\"m\").hasMessage(\"n\")\n\
    }\n";

fn classpath() -> Vec<PathBuf> {
    let sources = [
        ("Base.java".to_string(), BASE.to_string()),
        ("Mid.java".to_string(), MID.to_string()),
        ("ThrowableAssert.java".to_string(), THROWABLE.to_string()),
    ];
    let (library, _) =
        common::javac_compile(&sources, &[]).expect("javac must compile the self-type fixture");
    vec![library, common::stdlib_jar()]
}

#[test]
fn a_java_wildcard_self_type_keeps_its_declared_bound_like_kotlinc() {
    let classpath = classpath();
    let classes = common::classes_against_kotlinc_module_cp(&[("Use.kt", USE)], &classpath[..1]);
    assert_eq!(classes.differences(), Vec::<String>::new());

    let jdk = common::jdk_modules();
    let compiled = common::compile_in_process(USE, "Use", &classpath, Some(jdk.as_path()));
    assert!(
        compiled.is_some(),
        "the accepted fixture must lower and emit: {:?}",
        common::front_end_diagnostics(USE, &classpath, Some(jdk.as_path()))
    );
}
