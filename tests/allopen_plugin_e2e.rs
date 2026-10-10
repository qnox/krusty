//! kotlinc's all-open compiler plugin, as krusty's native pass.
//!
//! `-Xplugin=allopen-compiler-plugin.jar -P plugin:org.jetbrains.kotlin.allopen:annotation=<fqname>`
//! (or `preset=spring|micronaut|quarkus`) makes `open` the default modality of every class the
//! annotation matches and of the members it declares. A class matches through its own annotation, a
//! meta-annotation at any depth, or a supertype. Each fixture is a `box()` program: it overrides
//! members kotlinc only lets it override under the plugin, every class krusty emits must equal the
//! reference compiler's byte for byte, and `box()` must return `OK`.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::common;

const ALLOPEN_ID: &str = "org.jetbrains.kotlin.allopen";

fn allopen_jar() -> PathBuf {
    let jar = common::kotlinc_lib_dir()
        .expect("the reference kotlinc distribution is provisioned")
        .join("allopen-compiler-plugin.jar");
    assert!(
        jar.is_file(),
        "the reference distribution ships {}",
        jar.display()
    );
    jar
}

fn join_classpath(entries: &[PathBuf]) -> String {
    std::env::join_paths(entries)
        .expect("classpath entries contain no separator")
        .to_string_lossy()
        .into_owned()
}

/// The `.class` files under `dir`, keyed by their path relative to it, in path order.
fn classes_in(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.map(|entry| entry.expect("readable output entry").path()) {
            if entry.is_dir() {
                walk(base, &entry, out);
            } else if entry
                .extension()
                .is_some_and(|extension| extension == "class")
            {
                let relative = entry
                    .strip_prefix(base)
                    .expect("entry under the output")
                    .with_extension("");
                let name = relative.to_string_lossy().replace('\\', "/");
                out.push((name, std::fs::read(&entry).expect("read the class file")));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort_by(|left, right| left.0.cmp(&right.0));
    out
}

/// One compilation, built by kotlinc and by the krusty binary from the same sources and switches.
struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let dir = common::scratch_dir()
            .expect("a scratch directory")
            .join(format!("allopen-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the fixture directory");
        Fixture { dir }
    }

    fn write(&self, sources: &[(&str, &str)], unit: &str) -> Vec<String> {
        let source_dir = self.dir.join(unit).join("src");
        std::fs::create_dir_all(&source_dir).expect("create the source directory");
        sources
            .iter()
            .map(|(name, text)| {
                let path = source_dir.join(name);
                std::fs::write(&path, text).expect("write a fixture source");
                path.display().to_string()
            })
            .collect()
    }

    /// Build `sources` with kotlinc into `<unit>/kotlinc`.
    fn kotlinc(
        &self,
        unit: &str,
        sources: &[(&str, &str)],
        classpath: &[PathBuf],
        switches: &[String],
    ) -> PathBuf {
        let files = self.write(sources, unit);
        let out = self.dir.join(unit).join("kotlinc");
        let mut arguments = switches.to_vec();
        if !classpath.is_empty() {
            arguments.extend(["-cp".to_string(), join_classpath(classpath)]);
        }
        arguments.extend(["-d".to_string(), out.display().to_string()]);
        arguments.extend(files);
        let (code, diagnostics) =
            common::kotlinc_compile(&arguments).expect("the reference compiler runs");
        assert_eq!(code, 0, "kotlinc rejected {unit}: {diagnostics}");
        out
    }

    /// Build `sources` with the krusty binary into `<unit>/krusty`.
    fn krusty(
        &self,
        unit: &str,
        sources: &[(&str, &str)],
        classpath: &[PathBuf],
        switches: &[String],
    ) -> PathBuf {
        let files = self.write(sources, unit);
        let out = self.dir.join(unit).join("krusty");
        let mut command = Command::new(common::krusty_binary());
        if !classpath.is_empty() {
            command.args(["-cp", &join_classpath(classpath)]);
        }
        let result = command
            .args(switches)
            .arg("-d")
            .arg(&out)
            .args(files)
            .output()
            .expect("run krusty");
        assert_eq!(
            result.status.code(),
            Some(0),
            "krusty rejected {unit}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        out
    }
}

/// Every class kotlinc emitted, and no other, with identical bytes; then `box()` on krusty's output.
fn assert_same_classes_and_box(reference: &Path, krusty: &Path, runtime: &[PathBuf]) {
    let expected = classes_in(reference);
    let actual = classes_in(krusty);
    assert_eq!(
        actual.iter().map(|(name, _)| name).collect::<Vec<_>>(),
        expected.iter().map(|(name, _)| name).collect::<Vec<_>>(),
        "krusty emitted a different class set"
    );
    for ((name, actual), (_, expected)) in actual.iter().zip(&expected) {
        assert!(
            actual == expected,
            "{name}.class differs from kotlinc's under all-open"
        );
    }
    let box_class = common::find_box_class(&actual).expect("a class declaring box()");
    assert_eq!(
        common::run_box(&actual, &box_class, runtime).as_deref(),
        Some("OK")
    );
}

fn allopen_switches(options: &[&str]) -> Vec<String> {
    let mut switches = vec![format!("-Xplugin={}", allopen_jar().display())];
    for option in options {
        switches.extend(["-P".to_string(), format!("plugin:{ALLOPEN_ID}:{option}")]);
    }
    switches
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
    let fixture = Fixture::new("same-module");
    let sources = [("Main.kt", SAME_MODULE)];
    let switches = allopen_switches(&["annotation=AllOpen"]);
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
    let fixture = Fixture::new("spring-preset");
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
