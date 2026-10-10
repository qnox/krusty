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
