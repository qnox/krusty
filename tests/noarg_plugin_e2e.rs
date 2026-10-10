//! kotlinc's no-arg compiler plugin, as krusty's native pass.
//!
//! `-Xplugin=noarg-compiler-plugin.jar -P plugin:org.jetbrains.kotlin.noarg:annotation=<fqname>`
//! (or `preset=jpa`) gives every class the annotation matches a zero-argument constructor that
//! delegates to the superclass's and is hidden from Kotlin callers. A class matches through its own
//! annotation, a meta-annotation at any depth, or a supertype. Each fixture is a `box()` program
//! that instantiates the classes reflectively, as a framework does; every class krusty emits must
//! equal the reference compiler's byte for byte, and `box()` must return `OK`.

use super::common;
use super::compiler_plugin_fixture::{assert_same_classes_and_box, plugin_switches, PluginFixture};

const NOARG_ID: &str = "org.jetbrains.kotlin.noarg";

fn noarg_switches(options: &[&str]) -> Vec<String> {
    plugin_switches("noarg-compiler-plugin.jar", NOARG_ID, options)
}

const SAME_MODULE: &str = r#"annotation class NoArg

@NoArg
annotation class Entity

@Entity
annotation class Table

@NoArg
class Direct(val name: String, var count: Int)

@Table
class ViaMeta(val id: Long)

@NoArg
abstract class Base(val tag: String)

class Derived(tag: String, val extra: Int) : Base(tag)

open class Plain

@NoArg
class OfPlain(val p: Int) : Plain()

@NoArg
class SecondaryOnly {
    val made: String
    constructor(made: String) { this.made = made }
}

@NoArg
class HasNoArg(val v: Int) { constructor() : this(7) }

@NoArg
class AllDefaults(val v: Int = 3)

@NoArg
data class Point(val x: Int, val y: Int)

class Holder { @NoArg class Nested(val n: Int) }

@NoArg
open class Opened(val o: Int)

@NoArg
class Closed private constructor(val c: Int)

fun <T : Any> make(type: Class<T>): T = type.getDeclaredConstructor().newInstance()

fun box(): String {
    if (make(Direct::class.java).count != 0) return "direct"
    if (make(ViaMeta::class.java).id != 0L) return "meta"
    if (make(Derived::class.java).extra != 0) return "derived"
    if (make(OfPlain::class.java).p != 0) return "plain"
    if (make(HasNoArg::class.java).v != 7) return "declared"
    if (make(AllDefaults::class.java).v != 3) return "defaults"
    if (make(Point::class.java) != Point(0, 0)) return "data"
    if (make(Holder.Nested::class.java).n != 0) return "nested"
    if (make(Opened::class.java).o != 0) return "opened"
    if (make(Closed::class.java).c != 0) return "closed"
    val secondary: String? = make(SecondaryOnly::class.java).made
    return if (secondary == null) "OK" else "secondary"
}
"#;

/// Direct, meta-annotation and supertype matches; a class whose superclass gets the constructor; a
/// class with only secondary constructors; classes that already declare a constructor callable
/// without arguments and get none; a data class, whose generated members follow the constructor; a
/// nested class; a private primary constructor.
#[test]
fn no_arg_constructors_match_kotlinc() {
    let fixture = PluginFixture::new("noarg-same-module");
    let sources = [("Main.kt", SAME_MODULE)];
    let switches = noarg_switches(&["annotation=NoArg"]);
    let stdlib = vec![common::stdlib_jar()];
    let reference = fixture.kotlinc("main", &sources, &[], &switches);
    let krusty = fixture.krusty("main", &sources, &[], &switches);
    assert_same_classes_and_box(&reference, &krusty, &stdlib);
}

const JPA_LIBRARY: &[(&str, &str)] = &[
    (
        "Persistence.kt",
        "package javax.persistence\n\
         annotation class Entity\n\
         annotation class MappedSuperclass\n",
    ),
    (
        "Persistent.kt",
        "package lib\n\
         import javax.persistence.MappedSuperclass\n\
         @MappedSuperclass abstract class Persistent(val id: Long)\n",
    ),
];

const JPA_MAIN: &str = r#"import javax.persistence.Entity
import lib.Persistent

@Entity
class User(id: Long, val name: String) : Persistent(id)

class Admin(id: Long, val level: Int) : Persistent(id)

fun box(): String {
    val user = User::class.java.getDeclaredConstructor().newInstance()
    if (user.id != 0L || user.name != null) return "user"
    val admin = Admin::class.java.getDeclaredConstructor().newInstance()
    return if (admin.level == 0 && User(3, "u").id == 3L) "OK" else "admin"
}
"#;

/// The `jpa` preset through a dependency each compiler builds for itself with the plugin: a
/// subclass matches through its library superclass's annotation and delegates to the no-arg
/// constructor the library generated.
#[test]
fn the_jpa_preset_matches_through_a_dependency() {
    let fixture = PluginFixture::new("noarg-jpa-preset");
    let switches = noarg_switches(&["preset=jpa"]);
    let stdlib = common::stdlib_jar();
    let reference_lib = fixture.kotlinc("lib", JPA_LIBRARY, &[], &switches);
    let krusty_lib = fixture.krusty("lib", JPA_LIBRARY, &[], &switches);
    let sources = [("Main.kt", JPA_MAIN)];
    let reference = fixture.kotlinc("main", &sources, &[reference_lib], &switches);
    let krusty = fixture.krusty(
        "main",
        &sources,
        std::slice::from_ref(&krusty_lib),
        &switches,
    );
    assert_same_classes_and_box(&reference, &krusty, &[stdlib, krusty_lib]);
}

const REJECTED: &str = r#"annotation class NoArg

open class NeedsArg(val a: Int)

@NoArg
class Child(val b: Int) : NeedsArg(b)

class Outer { @NoArg inner class In(val c: Int) }

@NoArg
@JvmInline
value class Wrapped(val d: Int)
"#;

/// kotlinc's no-arg checker: a matched class whose superclass has no constructor callable without
/// arguments, and a matched inner class, are errors on the class's name. kotlinc 2.4.20 reports
/// nothing for a matched value class.
#[test]
fn the_plugins_errors_match_kotlinc() {
    let jar = super::compiler_plugin_fixture::kotlinc_plugin_jar("noarg-compiler-plugin.jar");
    let shared = [
        format!("-Xplugin={}", jar.display()),
        "-P".to_string(),
        format!("plugin:{NOARG_ID}:annotation=NoArg"),
    ];
    let result = common::compiler_diagnostics_with_shared_args(
        &[("Rejected.kt", REJECTED)],
        &[common::stdlib_jar()],
        &shared,
    );
    let render = |errors: Vec<common::CompilerError>| {
        errors
            .into_iter()
            .map(|error| {
                format!(
                    "{}:{}:{}: {}",
                    error.file, error.line, error.column, error.message
                )
            })
            .collect::<Vec<_>>()
    };
    let kotlinc = render(common::compiler_errors(&result.reference_stderr));
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    let krusty = render(krusty);
    assert_ne!(result.reference_code, 0, "kotlinc accepted the fixture");
    assert_ne!(result.krusty_code, 0, "krusty accepted the fixture");
    assert_eq!(
        kotlinc,
        vec![
            "Rejected.kt:6:7: zero-argument constructor was not found in the superclass.",
            "Rejected.kt:8:34: noarg constructor generation is not possible for inner classes.",
        ]
    );
    assert_eq!(
        krusty, kotlinc,
        "krusty: {}{}",
        result.krusty_stdout, result.krusty_stderr
    );
}
