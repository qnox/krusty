//! kotlinc's all-open compiler plugin, as krusty's native pass.
//!
//! `-Xplugin=allopen-compiler-plugin.jar -P plugin:org.jetbrains.kotlin.allopen:annotation=<fqname>`
//! (or `preset=spring|micronaut|quarkus`) makes `open` the default modality of every class the
//! annotation matches and of the members it declares. A class matches through its own annotation, a
//! meta-annotation at any depth, or a supertype. Each fixture is a `box()` program: it overrides
//! members kotlinc only lets it override under the plugin, every class krusty emits must equal the
//! reference compiler's byte for byte, and `box()` must return `OK`.

use super::common;
use super::compiler_plugin_fixture::{assert_same_classes_and_box, plugin_switches, PluginFixture};

const ALLOPEN_ID: &str = "org.jetbrains.kotlin.allopen";

fn allopen_switches(options: &[&str]) -> Vec<String> {
    plugin_switches("allopen-compiler-plugin.jar", ALLOPEN_ID, options)
}

const SAME_MODULE: &str = r#"annotation class AllOpen

@AllOpen
class Base {
    fun greet() = "base"
    val name: String = "b"
    var count: Int = 0
    final fun fixed() = "fixed"
    final val fixedName = "fixed"
    private fun secret() = "s"
    internal fun inside() = secret()
    companion object { fun make() = Base() }
}

class Derived : Base() {
    override fun greet() = "derived"
    override val name: String = "d"
}

@AllOpen
annotation class Component

@Component
annotation class Service

@Service
class Svc(val id: String) { fun describe() = "svc:$id" }

class SvcImpl : Svc("x") { override fun describe() = "impl" }

@CycleB
@AllOpen
annotation class CycleA

@CycleA
annotation class CycleB

// Visiting CycleA first reaches the configured annotation only after crossing CycleB's edge back
// to CycleA. That must not leave CycleB cached as unmatched for the later declaration.
@CycleA
class FirstCycle { fun first() = "first" }

@CycleB
class SecondCycle { fun second() = "second" }

class CycleImpl : SecondCycle() { override fun second() = "cycle" }

@AllOpen
abstract class Root { fun r() = "root" }

open class Mid : Root() { fun m() = "mid" }

class Leaf : Mid() {
    override fun m() = "leaf"
    override fun r() = "leafroot"
}

@AllOpen
final class Closed { fun c() = "c" }

@AllOpen interface Marked { fun dflt() = "d" }

class Impl : Marked { fun own() = "own" }

@AllOpen
object Single { fun s() = "s" }

@AllOpen
data class Data(val x: Int) { fun twice() = x * 2 }

class Plain { fun p() = "p" }

fun box(): String {
    val b: Base = Derived()
    if (b.greet() != "derived") return "greet"
    if (b.name != "d") return "name"
    if (b.inside() != "s") return "inside"
    val s: Svc = SvcImpl()
    if (s.describe() != "impl") return "describe"
    val cycle: SecondCycle = CycleImpl()
    if (FirstCycle().first() != "first" || cycle.second() != "cycle") return "cycle"
    val l: Mid = Leaf()
    if (l.m() != "leaf" || l.r() != "leafroot") return "leaf"
    if (Closed().c() != "c" || Impl().own() != "own" || Single.s() != "s") return "others"
    if (Data(2).twice() != 4 || Data(2) != Data(2) || Data(3).toString() != "Data(x=3)") return "data"
    if (Data(2).copy().component1() != 2 || Plain().p() != "p") return "copy"
    return if (Base.make().fixed() == "fixed") "OK" else "fixed"
}
"#;

/// Direct, meta- and supertype matches; explicit `final` members and classes; members of a final
/// class the annotation matches; interfaces, objects and companions left alone; a data class read
/// through its now-open getters; a private member that loses `ACC_FINAL`.
#[test]
fn all_open_classes_and_members_match_kotlinc() {
    let fixture = PluginFixture::new("same-module");
    let sources = [("Main.kt", SAME_MODULE)];
    let switches = allopen_switches(&["annotation=AllOpen"]);
    let stdlib = vec![common::stdlib_jar()];
    let reference = fixture.kotlinc("main", &sources, &[], &switches);
    let krusty = fixture.krusty("main", &sources, &[], &switches);
    assert_same_classes_and_box(&reference, &krusty, &stdlib);
}

/// The same program configured through kotlinc's modern syntax,
/// `-Xcompiler-plugin=<jar>=annotation=AllOpen`: its options reach the plugin its jar loads.
#[test]
fn modern_syntax_options_match_kotlinc() {
    let fixture = Fixture::new("modern-syntax");
    let sources = [("Main.kt", SAME_MODULE)];
    let switches = vec![format!(
        "-Xcompiler-plugin={}=annotation=AllOpen",
        allopen_jar().display()
    )];
    let stdlib = vec![common::stdlib_jar()];
    let reference = fixture.kotlinc("main", &sources, &[], &switches);
    let krusty = fixture.krusty("main", &sources, &[], &switches);
    assert_same_classes_and_box(&reference, &krusty, &stdlib);
}

const SPRING_LIBRARY: &[(&str, &str)] = &[
    (
        "Component.kt",
        "package org.springframework.stereotype\nannotation class Component\n",
    ),
    (
        "Service.kt",
        "package lib\n\
         import org.springframework.stereotype.Component\n\
         @Component annotation class Service\n\
         @Service abstract class Repository { open fun base() = \"base\" }\n",
    ),
];

const SPRING_MAIN: &str = r#"import lib.Repository
import lib.Service

@Service
class Users { fun find() = "users" }

class Accounts : Repository() { fun own() = "own" }

class AuditedAccounts : Accounts() {
    override fun own() = "audited"
    override fun base() = "audited-base"
}

class CachedUsers : Users() { override fun find() = "cached" }

fun box(): String {
    val accounts: Accounts = AuditedAccounts()
    if (accounts.own() != "audited" || accounts.base() != "audited-base") return "accounts"
    val users: Users = CachedUsers()
    return if (users.find() == "cached") "OK" else "users"
}
"#;

/// The `spring` preset, matched through annotations and a supertype that a dependency declares: a
/// meta-annotated annotation class and an annotated abstract class from a library each compiler
/// builds for itself.
#[test]
fn the_spring_preset_matches_through_a_dependency() {
    let fixture = PluginFixture::new("spring-preset");
    let switches = allopen_switches(&["preset=spring"]);
    let stdlib = common::stdlib_jar();
    let reference_lib = fixture.kotlinc("lib", SPRING_LIBRARY, &[], &[]);
    let krusty_lib = fixture.krusty("lib", SPRING_LIBRARY, &[], &[]);
    let sources = [("Main.kt", SPRING_MAIN)];
    let reference = fixture.kotlinc("main", &sources, &[reference_lib], &switches);
    let krusty = fixture.krusty(
        "main",
        &sources,
        std::slice::from_ref(&krusty_lib),
        &switches,
    );
    assert_same_classes_and_box(&reference, &krusty, &[stdlib, krusty_lib]);
}
