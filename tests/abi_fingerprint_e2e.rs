//! ABI fingerprinting: the property a Go-like build cache rests on.
//!
//! `docs/BUILD_AND_NATIVE_PLAN.md` proposes rebuilding a module's dependents only when that
//! module's ABI changes, rather than whenever any of its bytes change. The whole design is worth
//! nothing unless two things hold:
//!
//! * an edit confined to a method BODY leaves the ABI fingerprint unchanged (otherwise there is no
//!   avoidance — today a consumer "rebuilds on any change, not only on ABI changes",
//!   `crates/krusty-cli/src/worker.rs`), and
//! * every edit that a dependent can OBSERVE changes the fingerprint (otherwise the cache serves a
//!   stale artifact, which ships a wrong binary with no diagnostic).
//!
//! These tests demonstrate both against real emitted bytes, using krusty's own class reader
//! ([`krusty::jvm::classreader::parse_class`]) as the extraction primitive — it yields signatures,
//! `ConstantValue`s and decoded `@kotlin.Metadata` while carrying no method bodies, which is
//! exactly the ABI/Impl cut the document describes.
//!
//! This is a proof of concept, not the proposed implementation. A real `krusty-build::abi` must
//! additionally carry the compiled bodies of `inline` functions (on this target an inline body is
//! relocated bytecode, `src/jvm/inline.rs`, so the artifact spans the backend), contracts, and
//! separate public/friend hashes for `internal`. Those are called out in the proposal; the point
//! here is that the central premise is real and measurable, and that the OBVIOUS definition of an
//! ABI is already unsound — see `const_value_edit_changes_the_fingerprint`.

use super::common;

/// FNV-1a, matching `crates/krusty-lsp/src/project/fingerprint.rs`. Hand-rolled because the project
/// is deliberately dependency-lean; a real implementation would want a cryptographic hash, since a
/// build cache key is attacker-reachable once shared between machines.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

/// What an ABI fingerprint is allowed to see. Both variants exclude method bodies; `Signatures`
/// additionally drops field `ConstantValue`s, which is the naive reading of "declaration signatures
/// only" and is demonstrated below to be unsound.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Surface {
    /// Signatures, supertypes, flags and `@kotlin.Metadata` — no constant values.
    Signatures,
    /// The above plus every `static final` field's `ConstantValue`.
    SignaturesAndConstants,
}

/// Render the ABI surface of one emitted class set and hash it.
///
/// Member order is preserved rather than sorted. It is deterministic (see
/// `emission_determinism_e2e`), and preserving it is the conservative choice: a pure member
/// reordering then invalidates dependents unnecessarily, which costs a rebuild but can never serve
/// a stale artifact. A real implementation may sort once it is proven that nothing a dependent
/// observes depends on declaration order.
fn abi_fingerprint(classes: &[(String, Vec<u8>)], surface: Surface) -> u64 {
    let mut rendered = String::new();
    let mut ordered: Vec<&(String, Vec<u8>)> = classes
        .iter()
        .filter(|(name, _)| !name.ends_with(".kotlin_module"))
        .collect();
    ordered.sort_by(|a, b| a.0.cmp(&b.0));

    for (name, bytes) in ordered {
        let info = krusty::jvm::classreader::parse_class(bytes)
            .unwrap_or_else(|error| panic!("parse {name}: {error:?}"));
        rendered.push_str(&format!(
            "class {} access={} super={} interfaces=[{}] retention={:?} meta={:?}\n",
            info.this_class,
            info.access,
            info.super_class
                .map(|s| s.to_string())
                .unwrap_or_else(|| "-".into()),
            info.interfaces
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(","),
            info.retention,
            info.meta,
        ));
        for field in &info.fields {
            rendered.push_str(&format!(
                "  field {} {} access={} signature={:?}",
                field.name, field.descriptor, field.access, field.signature
            ));
            if surface == Surface::SignaturesAndConstants {
                rendered.push_str(&format!(" const={:?}", field.const_value));
            }
            rendered.push('\n');
        }
        for method in &info.methods {
            rendered.push_str(&format!(
                "  method {} {} access={} signature={:?}\n",
                method.name, method.descriptor, method.access, method.signature
            ));
        }
    }
    fnv1a(rendered.as_bytes())
}

/// One `lib` module whose method body, member set and `const val` can each be varied independently.
fn library(body: &str, limit: i32, extra_member: bool) -> String {
    let extra = if extra_member {
        "    fun extra(): Int = 7\n"
    } else {
        ""
    };
    format!(
        "package lib\n\
         const val LIMIT: Int = {limit}\n\
         class Api {{\n\
         \x20   fun compute(x: Int): Int {{ return {body} }}\n\
         \x20   fun name(): String = \"api\"\n\
         {extra}\
         }}\n\
         fun box(): String = \"OK\"\n"
    )
}

fn compile(src: &str) -> Vec<(String, Vec<u8>)> {
    let jdk = common::jdk_modules();
    let classpath = vec![common::stdlib_jar()];
    common::compile_in_process(src, "Lib", &classpath, Some(jdk.as_path())).unwrap_or_else(|| {
        panic!(
            "ABI fixture must compile:\n{src}\n{:?}",
            common::front_end_diagnostics(src, &classpath, Some(jdk.as_path()))
        )
    })
}

/// Concatenated raw bytes, for asserting that an edit really did change the emitted output.
fn raw_bytes(classes: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut ordered: Vec<&(String, Vec<u8>)> = classes.iter().collect();
    ordered.sort_by(|a, b| a.0.cmp(&b.0));
    ordered.iter().flat_map(|(_, b)| b.clone()).collect()
}

/// The avoidance premise: a body-only edit changes emitted bytes but not the ABI.
///
/// Without this there is nothing to build — every edit would rebuild every dependent, which is
/// exactly today's behavior.
#[test]
fn body_edit_preserves_the_fingerprint_while_changing_bytes() {
    let before = compile(&library("x + 1", 30, false));
    let after = compile(&library("x + 2", 30, false));

    assert_ne!(
        raw_bytes(&before),
        raw_bytes(&after),
        "a body edit must change emitted bytes, or this fixture proves nothing"
    );
    assert_eq!(
        abi_fingerprint(&before, Surface::SignaturesAndConstants),
        abi_fingerprint(&after, Surface::SignaturesAndConstants),
        "a body-only edit must NOT change the ABI fingerprint — otherwise the cache has no \
         avoidance and every dependent rebuilds on every edit"
    );
}

/// The soundness premise, part one: adding a member changes the ABI.
#[test]
fn signature_edit_changes_the_fingerprint() {
    let before = compile(&library("x + 1", 30, false));
    let after = compile(&library("x + 1", 30, true));

    assert_ne!(
        abi_fingerprint(&before, Surface::SignaturesAndConstants),
        abi_fingerprint(&after, Surface::SignaturesAndConstants),
        "adding a member must change the ABI fingerprint"
    );
}

/// The soundness premise, part two — and the reason "declaration signatures only" is the wrong
/// definition of an ABI.
///
/// A `const val` is folded into the CONSUMER: `src/jvm/classreader.rs` reads the `static final`
/// field's `ConstantValue`, `src/jvm/jvm_libraries.rs` turns it into a `LibraryConst`, and
/// `src/fir/body_check.rs` publishes it as a `FirExprKind::Constant` on the read. So changing
/// `LIMIT` from 30 to 99 changes what every dependent compiles to, while changing no signature.
///
/// Measured independently with `javap` before this test was written: `javap -s` output is
/// byte-identical across the two versions, and only `javap -s -constants` differs. A fingerprint
/// over signatures alone therefore COLLIDES — the cache would serve the stale artifact and the
/// dependent would keep the old constant, silently. Both halves are asserted here so the trap
/// cannot quietly reappear.
#[test]
fn const_value_edit_changes_the_fingerprint() {
    let before = compile(&library("x + 1", 30, false));
    let after = compile(&library("x + 1", 99, false));

    assert_ne!(
        raw_bytes(&before),
        raw_bytes(&after),
        "a const edit must change emitted bytes"
    );
    assert_eq!(
        abi_fingerprint(&before, Surface::Signatures),
        abi_fingerprint(&after, Surface::Signatures),
        "documenting the trap: over signatures alone the two versions are indistinguishable, so a \
         naive ABI hash serves a stale artifact and the dependent keeps the old constant"
    );
    assert_ne!(
        abi_fingerprint(&before, Surface::SignaturesAndConstants),
        abi_fingerprint(&after, Surface::SignaturesAndConstants),
        "including ConstantValue must make the const edit observable"
    );
}

/// The fingerprint is stable across repeated compilation of identical input — it inherits the
/// emission determinism gated by `emission_determinism_e2e`, through the class reader.
#[test]
fn fingerprint_is_stable_across_recompilation() {
    let src = library("x + 1", 30, false);
    let first = abi_fingerprint(&compile(&src), Surface::SignaturesAndConstants);
    for run in 2..=4 {
        assert_eq!(
            first,
            abi_fingerprint(&compile(&src), Surface::SignaturesAndConstants),
            "fingerprint differs on run {run} of identical input"
        );
    }
}
