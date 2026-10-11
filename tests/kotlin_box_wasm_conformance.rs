//! The `codegen/box` corpus through krusty's WebAssembly backend, once per kotlinc Wasm target.
//!
//! Each case is compiled into one WasmGC module whose `_start` prints `box()`'s answer, plus the
//! `.mjs` loader that runs it, and RUN under Node.js: `wasm-js` through the loader's JavaScript
//! imports, `wasm-wasi` through Node's WASI preview-1 host. The driver, the verdict and the
//! committed ratchet are every runnable target's (`box_lane`); this file is what is Wasm's.
//!
//! The two targets are separate lanes with separate inventories because the corpus treats them
//! separately — `// IGNORE_BACKEND: WASM_WASI` mutes a case on one only — and because the host
//! boundary that separates them is exactly where they can disagree.

use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

use krusty::backend::Entry;
use krusty::conformance::TestTarget;
use krusty::wasm::{WasmBackend, WasmTarget};

use super::box_lane::{self, Lane, Outcome};
use super::box_ratchet::Platform;

/// The prefix the wasm backend puts on every decline; what follows names the construct.
const DECLINE_PREFIX: &str = "krusty: the wasm backend does not support ";

/// The oldest Node.js whose WebAssembly runs WasmGC modules without flags.
const MINIMUM_NODE: u32 = 22;

struct WasmLane {
    target: WasmTarget,
}

impl WasmLane {
    /// The directive tokens that name this target: `WASM` names both.
    fn tokens(&self) -> [&'static str; 2] {
        ["WASM", self.target.backend_token()]
    }
}

/// Why this host has no Node.js able to run the modules, or `None` when it has one.
fn node_unavailable() -> Option<String> {
    static NODE: OnceLock<Option<String>> = OnceLock::new();
    NODE.get_or_init(|| {
        let output = match Command::new("node").arg("--version").output() {
            Ok(output) if output.status.success() => output,
            _ => return Some("no `node` on PATH".to_string()),
        };
        let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let major = version
            .trim_start_matches('v')
            .split('.')
            .next()
            .and_then(|major| major.parse::<u32>().ok());
        match major {
            Some(major) if major >= MINIMUM_NODE => None,
            _ => Some(format!(
                "node {version} predates WasmGC support (needs {MINIMUM_NODE} or newer)"
            )),
        }
    })
    .clone()
}

impl Lane for WasmLane {
    fn platform(&self) -> Platform {
        match self.target {
            WasmTarget::Js => Platform::WasmJs,
            WasmTarget::Wasi => Platform::WasmWasi,
        }
    }

    fn label(&self) -> &'static str {
        self.target.name()
    }

    fn env_infix(&self) -> &'static str {
        self.target.backend_token()
    }

    fn test_target(&self) -> TestTarget {
        match self.target {
            WasmTarget::Js => TestTarget::WasmJs,
            WasmTarget::Wasi => TestTarget::WasmWasi,
        }
    }

    fn unavailable(&self) -> Option<String> {
        if !box_lane::frontend_available() {
            return Some("no stdlib and JDK modules to analyze against".to_string());
        }
        node_unavailable()
    }

    fn not_applicable(&self, src: &str) -> Option<&'static str> {
        if !krusty::conformance::backend_targeted(src, &self.tokens()) {
            return Some("targeted at another backend");
        }
        if krusty::conformance::backend_muted(src, &self.tokens()) {
            return Some("muted on this wasm target");
        }
        if krusty::conformance::directive(src, "FULL_JDK") {
            return Some("requires the JDK runtime");
        }
        None
    }

    fn harness_limitation(&self, src: &str) -> Option<&'static str> {
        if src.contains("// MODULE:") {
            return Some("multi-module compilation is not wired to the module dependency provider");
        }
        None
    }

    fn compile_and_run(
        &self,
        scratch: &Path,
        stem: &str,
        sources: &[(String, String)],
        directive_source: &str,
    ) -> Outcome {
        let backend = WasmBackend::new(self.target).with_entry(Entry::Box);
        let artifacts = match box_lane::compile(
            sources,
            directive_source,
            self.test_target(),
            &backend,
            "box",
            DECLINE_PREFIX,
        ) {
            Ok(artifacts) => artifacts,
            Err(outcome) => return outcome,
        };
        if artifacts.is_empty() {
            return Outcome::Frontend(
                "the frontend produced no checked file and said nothing".to_string(),
            );
        }
        let directory = scratch.join(format!(
            "{stem}-{}-{}-{}",
            self.target.name(),
            std::process::id(),
            rayon::current_thread_index().unwrap_or(0)
        ));
        if let Err(error) = std::fs::create_dir_all(&directory) {
            return Outcome::Failed(format!("creating the case directory: {error}"));
        }
        for (name, bytes) in &artifacts {
            if let Err(error) = std::fs::write(directory.join(name), bytes) {
                return Outcome::Failed(format!("writing {name}: {error}"));
            }
        }
        let outcome = box_lane::run_program(
            Command::new("node")
                .arg("--no-warnings")
                .arg(directory.join("box.mjs")),
        );
        let _ = std::fs::remove_dir_all(&directory);
        outcome
    }
}

#[test]
fn wasm_applicability_reads_both_the_family_and_the_exact_target() {
    let js = WasmLane {
        target: WasmTarget::Js,
    };
    let wasi = WasmLane {
        target: WasmTarget::Wasi,
    };
    let muted_on_wasi = "// IGNORE_BACKEND: WASM_WASI\nfun box() = \"OK\"";
    assert_eq!(js.not_applicable(muted_on_wasi), None);
    assert_eq!(
        wasi.not_applicable(muted_on_wasi),
        Some("muted on this wasm target")
    );
    let wasm_only = "// TARGET_BACKEND: WASM\nfun box() = \"OK\"";
    assert_eq!(js.not_applicable(wasm_only), None);
    assert_eq!(wasi.not_applicable(wasm_only), None);
    assert_eq!(
        js.not_applicable("// TARGET_BACKEND: JVM\nfun box() = \"OK\""),
        Some("targeted at another backend")
    );
    assert_eq!(
        js.not_applicable("// IGNORE_BACKEND: JVM\nfun box() = \"OK\""),
        None,
        "a JVM mute must not remove Wasm coverage"
    );
}

#[test]
fn kotlin_codegen_box_wasm_js_conformance() {
    box_lane::run(&WasmLane {
        target: WasmTarget::Js,
    });
}

#[test]
fn kotlin_codegen_box_wasm_wasi_conformance() {
    box_lane::run(&WasmLane {
        target: WasmTarget::Wasi,
    });
}

/// Run `src` on both Wasm targets after the JVM oracle has answered `OK` for it; every target
/// must lower it and answer `OK` too. Skips where the lanes themselves cannot run.
fn expect_wasm_box(src: &str, stem: &str) {
    super::common::expect_box_ok_with_stdlib(src, stem);
    for target in WasmTarget::ALL {
        let lane = WasmLane { target };
        if lane.unavailable().is_some() {
            return;
        }
        let outcome = lane.compile_and_run(
            &std::env::temp_dir(),
            stem,
            &[(format!("{stem}.kt"), src.to_string())],
            src,
        );
        assert_eq!(outcome, Outcome::Pass, "{stem} on {}", target.name());
    }
}

/// Require both Wasm targets to decline `src` with exactly `reason`.
fn expect_wasm_decline(src: &str, stem: &str, reason: &str) {
    for target in WasmTarget::ALL {
        let lane = WasmLane { target };
        if lane.unavailable().is_some() {
            return;
        }
        let outcome = lane.compile_and_run(
            &std::env::temp_dir(),
            stem,
            &[(format!("{stem}.kt"), src.to_string())],
            src,
        );
        assert_eq!(
            outcome,
            Outcome::Declined(reason.to_string()),
            "{stem} on {}",
            target.name()
        );
    }
}

#[test]
fn wasm_classes_construct_store_fields_and_dispatch_virtually() {
    expect_wasm_box(
        r#"
open class Shape(val sides: Int) {
    var name: String = "shape"
    constructor(name: String, sides: Int) : this(sides) { this.name = name }
    open fun area(): Int = 0
    fun describe(): Int = area() * 10 + sides
}
class Square(val side: Int) : Shape("square", 4) {
    override fun area(): Int = side * side
}
open class Counter {
    var count = 0
    open fun step(): Int { count += 1; return count }
}
class Doubler : Counter() {
    override fun step(): Int = super.step() * 2
}
fun box(): String {
    val shape: Shape = Square(3)
    if (shape.describe() != 94) return "fail: describe ${shape.describe()}"
    if (shape.name != "square") return "fail: name ${shape.name}"
    val plain = Shape(5)
    if (plain.describe() != 5 || plain.name != "shape") return "fail: plain"
    plain.name = "pentagon"
    if (plain.name != "pentagon") return "fail: write"
    val counter: Counter = Doubler()
    counter.step()
    if (counter.step() != 4 || counter.count != 2) return "fail: super"
    return "OK"
}
"#,
        "wasmClassesDispatch",
    );
}

#[test]
fn wasm_type_tests_and_casts_follow_the_class_hierarchy() {
    expect_wasm_box(
        r#"
interface Named { fun name(): String }
open class Animal
class Cat : Animal(), Named { override fun name() = "cat" }
class Rock : Named { override fun name() = "rock" }
fun nameOf(value: Any?): String {
    if (value is Named) return value.name()
    return "none"
}
fun box(): String {
    val cat: Any = Cat()
    val animal: Animal = Animal()
    if (cat !is Animal || animal is Cat) return "fail: is"
    if (nameOf(cat) != "cat" || nameOf(Rock()) != "rock" || nameOf(animal) != "none") return "fail: interface"
    if (nameOf(null) != "none" || nameOf("text") != "none") return "fail: other"
    val unknown: Any = Cat()
    if ((animal as? Cat) != null || (unknown as? Cat) == null) return "fail: as?"
    val named = cat as Named
    if (named.name() != "cat") return "fail: as"
    val nothing: Any? = null
    if ((nothing as Cat?) != null) return "fail: nullable as"
    return "OK"
}
"#,
        "wasmClassesTypeTests",
    );
}

#[test]
fn wasm_object_declarations_are_built_once_on_first_use() {
    expect_wasm_box(
        r#"
var built = 0
object Registry {
    var size = 0
    init { built += 1 }
    fun add(): Int { size += 1; return size }
}
fun box(): String {
    if (built != 0) return "fail: eager"
    Registry.add()
    if (Registry.add() != 2 || built != 1) return "fail: ${Registry.size} $built"
    return "OK"
}
"#,
        "wasmClassesObjects",
    );
}

#[test]
fn wasm_class_equality_and_rendering_use_the_class_own_members() {
    expect_wasm_box(
        r#"
class Point(val x: Int, val y: Int) {
    override fun equals(other: Any?): Boolean = other is Point && other.x == x && other.y == y
    override fun hashCode(): Int = x * 31 + y
    override fun toString(): String = "Point"
}
class Plain
fun box(): String {
    val a = Point(1, 2)
    if (a != Point(1, 2) || a == Point(2, 1)) return "fail: equals"
    val p = Plain()
    if (p != p || p == Plain()) return "fail: identity"
    if ("$a" != "Point") return "fail: toString"
    return "OK"
}
"#,
        "wasmClassesAnyMembers",
    );
}

#[test]
fn wasm_declines_kotlin_any_rendering_it_has_no_body_for() {
    expect_wasm_decline(
        "class Plain\nfun box(): String = if (\"${Plain()}\" == \"\") \"fail\" else \"OK\"",
        "wasmClassesDefaultToString",
        "`kotlin.Any.toString`'s own rendering",
    );
}

#[test]
fn wasm_declines_an_enum_class_by_name() {
    expect_wasm_decline(
        "enum class Color { RED }\nfun box(): String = \"OK\"",
        "wasmClassesEnum",
        "an enum class (`Color`)",
    );
}
