//! A nested class calling a protected member of a classpath supertype.
//!
//! The JVM grants a protected member to its package and to a subclass calling it on its own
//! receiver. An anonymous class inside that subclass is neither, so a cross-package call goes
//! through `access$<name>` on the subclass. The same call inside the subclass's own method, and
//! any call from the member's package, stays a direct `invokevirtual`.

use super::common;

const BASE: &str = "package lib;\n\
public class Base {\n\
    protected String secret() { return \"O\"; }\n\
    protected String step(int n) { return n == 1 ? \"K\" : \"?\"; }\n\
}\n";

const HOST: &str = "import lib.Base\n\
\n\
class Host : Base() {\n\
    fun own(): String = secret()\n\
\n\
    fun go(): String {\n\
        val probe = object : Runnable {\n\
            override fun run() {}\n\
            fun read(): String = secret() + step(1)\n\
        }\n\
        return probe.read()\n\
    }\n\
}\n\
\n\
fun box(): String {\n\
    val host = Host()\n\
    val result = host.own() + host.go()\n\
    return if (result == \"OOK\") \"OK\" else result\n\
}\n";

const NEAR: &str = "public class Near {\n\
    protected String secret() { return \"OK\"; }\n\
}\n";

const SAME: &str = "class Same : Near() {\n\
    fun go(): String {\n\
        val probe = object {\n\
            fun read(): String = secret()\n\
        }\n\
        return probe.read()\n\
    }\n\
}\n\
\n\
fun box(): String = Same().go()\n";

const KOTLIN_BASE: &str = "package namedlib\n\
open class NamedBase {\n\
    protected fun step(named: Int): String = named.toString()\n\
}\n";

const NAMED_HOST: &str = "import namedlib.NamedBase\n\
class NamedHost : NamedBase() {\n\
    fun go(): String {\n\
        val probe = object { fun read(): String = step(1) }\n\
        return probe.read()\n\
    }\n\
}\n";

fn instructions(
    comparison: &common::ReferenceComparison,
    marker: &str,
) -> (Vec<String>, Vec<String>) {
    (
        common::method_instructions(&comparison.reference, marker),
        common::method_instructions(&comparison.krusty, marker),
    )
}

#[test]
fn a_nested_class_reaches_a_cross_package_protected_member_through_the_subclass() {
    let (classes, _) = common::javac_compile(&[("Base.java".into(), BASE.into())], &[])
        .expect("javac compiles the protected base");
    let host =
        common::compare_with_kotlinc_plugin("Host", HOST, "Host", &[classes.clone()], "1.8", &[])
            .expect("Host compiles against the Java base");
    for header in [
        "public static final java.lang.String access$secret(Host);",
        "public static final java.lang.String access$step(Host, int);",
    ] {
        let accessor = common::method_block(&host.reference, header);
        assert!(!accessor.is_empty(), "kotlinc declares {header}");
        assert_eq!(
            common::method_block(&host.krusty, header),
            accessor,
            "{header}"
        );
    }
    let (reference, krusty) = instructions(&host, "String own();");
    assert_eq!(krusty, reference, "Host.own stays a direct call");

    let nested = common::compare_with_kotlinc_plugin(
        "Host",
        HOST,
        "Host$go$probe$1",
        &[classes.clone()],
        "1.8",
        &[],
    )
    .expect("the anonymous class compiles");
    let (reference, krusty) = instructions(&nested, "String read();");
    assert_eq!(krusty, reference, "Host$go$probe$1.read");

    let jdk = common::jdk_modules();
    let classpath = vec![classes, common::stdlib_jar()];
    assert_eq!(
        common::expect_box_run(HOST, "Host", &classpath, Some(jdk.as_path())),
        "OK"
    );
}

#[test]
fn a_same_package_nested_class_calls_a_protected_member_directly() {
    let (classes, _) = common::javac_compile(&[("Near.java".into(), NEAR.into())], &[])
        .expect("javac compiles the same-package base");
    let nested = common::compare_with_kotlinc_plugin(
        "Same",
        SAME,
        "Same$go$probe$1",
        &[classes.clone()],
        "1.8",
        &[],
    )
    .expect("the same-package anonymous class compiles");
    let (reference, krusty) = instructions(&nested, "String read();");
    assert_eq!(krusty, reference, "Same$go$probe$1.read");
    assert!(
        !krusty
            .iter()
            .any(|instruction| instruction.contains("access$")),
        "a same-package caller needs no accessor: {krusty:?}"
    );

    let owner =
        common::compare_with_kotlinc_plugin("Same", SAME, "Same", &[classes.clone()], "1.8", &[])
            .expect("Same compiles");
    assert!(
        common::method_block(
            &owner.krusty,
            "public static final java.lang.String access$secret(Same);"
        )
        .is_empty(),
        "Same declares no access$secret"
    );

    let jdk = common::jdk_modules();
    let classpath = vec![classes, common::stdlib_jar()];
    assert_eq!(
        common::expect_box_run(SAME, "Same", &classpath, Some(jdk.as_path())),
        "OK"
    );
}

#[test]
fn a_dependency_parameter_identity_names_the_synthetic_access_bridge_local() {
    let classes = common::classes_against_kotlinc_lib_target(
        "NamedHost",
        &[("NamedBase.kt", KOTLIN_BASE)],
        NAMED_HOST,
        Some(8),
    )
    .expect("the Kotlin dependency and both consumers compile");
    let declaration = "public static final java.lang.String access$step(NamedHost, int);";
    let (reference, krusty) = classes.method_listing("NamedHost", declaration);
    assert_eq!(krusty, reference, "{declaration}");
}
