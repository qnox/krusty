//! The ABI surface of a module, and its fingerprint.
//!
//! A dependent must rebuild when a dependency's ABI changes and must NOT rebuild otherwise. Both
//! halves matter: without the first the cache serves a stale artifact and ships a wrong binary with
//! no diagnostic; without the second there is no avoidance at all, which is today's behavior
//! (`crates/krusty-cli/src/worker.rs`: a consumer "rebuilds on any change, not only on ABI
//! changes").
//!
//! **Method bodies are excluded by construction.** [`AbiMember`] has nowhere to put one. That is
//! the ABI/Impl split expressed as a type rather than as a filtering step that could be forgotten.
//!
//! # "Declaration signatures only" is the wrong definition
//!
//! A `const val` is folded into the CONSUMER: `src/jvm/classreader.rs` reads the `static final`
//! field's `ConstantValue`, `src/jvm/jvm_libraries.rs` turns it into a `LibraryConst`, and
//! `src/fir/body_check.rs` publishes it as a `FirExprKind::Constant` on the read. So changing a
//! constant changes what every dependent compiles to while changing no signature. Measured with
//! `javap` on krusty output: for `const val LIMIT` at 30 versus 99, `javap -s` is byte-identical
//! and only `javap -s -constants` differs. A fingerprint over signatures alone therefore collides.
//! [`AbiMember::constant`] is why this one does not; `tests/abi_fingerprint_e2e.rs` asserts both
//! halves against real emitted bytes.
//!
//! # What this does not yet carry
//!
//! * **`inline` function bodies.** On the JVM an inline body is relocated bytecode spliced into the
//!   call site (`src/jvm/inline.rs`), so it is genuinely part of the ABI and a real artifact must
//!   ship the compiled method. That makes ABI extraction span the backend, not the frontend alone.
//! * **Contracts.** A `contract { … }` block lives syntactically inside a body but drives callers'
//!   smart-cast analysis (`src/contracts.rs`), so a "body-only" edit to a function with a contract
//!   is not body-only.
//! * **Separate public and friend hashes.** Friendship is exact path-set membership, so pruning
//!   `internal` breaks friend modules while keeping it over-invalidates everyone else. Two hashes
//!   per module, with each edge selecting one.
//!
//! Until those land, a fingerprint from this module is sound for avoidance only on code with no
//! `inline` functions and no contracts. [`AbiClass::from_class_file`] cannot detect that for you.

use crate::fnv1a;

/// Whether an [`AbiMember`] came from a field or a method.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MemberKind {
    Field,
    Method,
}

impl MemberKind {
    fn tag(self) -> &'static str {
        match self {
            Self::Field => "field",
            Self::Method => "method",
        }
    }
}

/// One field or method, as a dependent sees it. There is deliberately no body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AbiMember {
    pub kind: MemberKind,
    pub name: String,
    /// JVM descriptor — the erased signature.
    pub descriptor: String,
    /// JVM access flags.
    pub access: u16,
    /// The generic `Signature` attribute, which carries what the erased descriptor drops.
    pub signature: Option<String>,
    /// A `static final` field's compile-time `ConstantValue`, rendered. `None` for methods and for
    /// non-constant fields. Present because a constant is inlined into consumers — see module docs.
    pub constant: Option<String>,
}

/// One class's ABI surface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AbiClass {
    /// Internal name, e.g. `lib/Api`.
    pub name: String,
    pub access: u16,
    pub super_name: Option<String>,
    pub interfaces: Vec<String>,
    /// The generic `Signature` attribute of the class itself.
    pub signature: Option<String>,
    /// For an annotation type, its retention. `SOURCE` annotations are invisible to dependents;
    /// `BINARY`/`RUNTIME` are not, so the distinction is ABI.
    pub retention: Option<String>,
    /// `@kotlin.Metadata`, rendered. On the JVM this IS the Kotlin ABI — it is what
    /// `src/jvm/jvm_libraries.rs` reads to type-check a dependent against this class — so it
    /// belongs in the fingerprint rather than beside it.
    pub metadata: String,
    /// Members in declaration order. Order is preserved rather than sorted: it is deterministic
    /// (gated by `tests/emission_determinism_e2e.rs`), and preserving it is conservative — a pure
    /// member reordering then invalidates dependents unnecessarily, which costs a rebuild but can
    /// never serve a stale artifact.
    pub members: Vec<AbiMember>,
}

/// A module's ABI condensed to one value. Folded into every dependent's cache key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AbiFingerprint(u64);

impl AbiFingerprint {
    pub fn value(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for AbiFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

impl AbiClass {
    /// Extract the ABI surface from one emitted `.class` file.
    ///
    /// Uses the compiler's own class reader, which yields signatures, `ConstantValue`s and decoded
    /// `@kotlin.Metadata` while never materializing a method body.
    pub fn from_class_file(bytes: &[u8]) -> Result<Self, String> {
        let info = krusty::jvm::classreader::parse_class(bytes)
            .map_err(|error| format!("class parse failed: {error:?}"))?;

        let mut members = Vec::with_capacity(info.fields.len() + info.methods.len());
        for field in &info.fields {
            members.push(AbiMember {
                kind: MemberKind::Field,
                name: field.name.clone(),
                descriptor: field.descriptor.clone(),
                access: field.access,
                signature: field.signature.clone(),
                constant: field.const_value.as_ref().map(|c| format!("{c:?}")),
            });
        }
        for method in &info.methods {
            members.push(AbiMember {
                kind: MemberKind::Method,
                name: method.name.clone(),
                descriptor: method.descriptor.clone(),
                access: method.access,
                signature: method.signature.clone(),
                constant: None,
            });
        }

        Ok(Self {
            name: info.this_class.to_string(),
            access: info.access,
            super_name: info.super_class.map(|s| s.to_string()),
            interfaces: info.interfaces.iter().map(|i| i.to_string()).collect(),
            signature: info.signature.clone(),
            retention: info.retention.clone(),
            metadata: format!("{:?}", info.meta),
            members,
        })
    }

    /// Canonical rendering. The fingerprint is a hash of this, so it is also the diff a human reads
    /// when asking why a rebuild triggered.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "class {} access={} super={} interfaces=[{}] signature={:?} retention={:?} meta={}\n",
            self.name,
            self.access,
            self.super_name.as_deref().unwrap_or("-"),
            self.interfaces.join(","),
            self.signature,
            self.retention,
            self.metadata,
        ));
        for member in &self.members {
            out.push_str(&format!(
                "  {} {} {} access={} signature={:?} constant={:?}\n",
                member.kind.tag(),
                member.name,
                member.descriptor,
                member.access,
                member.signature,
                member.constant,
            ));
        }
        out
    }
}

/// Fingerprint a module's classes.
///
/// Classes are sorted by name first, so the fingerprint does not depend on the order a driver
/// happened to collect them in. Members within a class keep declaration order — see
/// [`AbiClass::members`].
pub fn fingerprint(classes: &[AbiClass]) -> AbiFingerprint {
    let mut ordered: Vec<&AbiClass> = classes.iter().collect();
    ordered.sort_by(|a, b| a.name.cmp(&b.name));
    let mut rendered = String::new();
    for class in ordered {
        rendered.push_str(&class.render());
    }
    AbiFingerprint(fnv1a(rendered.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn method(name: &str, descriptor: &str) -> AbiMember {
        AbiMember {
            kind: MemberKind::Method,
            name: name.into(),
            descriptor: descriptor.into(),
            access: 1,
            signature: None,
            constant: None,
        }
    }

    fn constant_field(name: &str, value: &str) -> AbiMember {
        AbiMember {
            kind: MemberKind::Field,
            name: name.into(),
            descriptor: "I".into(),
            access: 25,
            signature: None,
            constant: Some(value.into()),
        }
    }

    fn class(name: &str, members: Vec<AbiMember>) -> AbiClass {
        AbiClass {
            name: name.into(),
            access: 33,
            super_name: Some("java/lang/Object".into()),
            interfaces: vec![],
            signature: None,
            retention: None,
            metadata: "meta".into(),
            members,
        }
    }

    #[test]
    fn identical_surfaces_fingerprint_identically() {
        let a = class("lib/Api", vec![method("compute", "(I)I")]);
        let b = class("lib/Api", vec![method("compute", "(I)I")]);
        assert_eq!(fingerprint(&[a]), fingerprint(&[b]));
    }

    #[test]
    fn class_collection_order_does_not_change_the_fingerprint() {
        let x = class("lib/A", vec![method("f", "()V")]);
        let y = class("lib/B", vec![method("g", "()V")]);
        assert_eq!(
            fingerprint(&[x.clone(), y.clone()]),
            fingerprint(&[y, x]),
            "a driver's collection order is not ABI"
        );
    }

    #[test]
    fn adding_a_member_changes_the_fingerprint() {
        let before = class("lib/Api", vec![method("compute", "(I)I")]);
        let after = class(
            "lib/Api",
            vec![method("compute", "(I)I"), method("extra", "()I")],
        );
        assert_ne!(fingerprint(&[before]), fingerprint(&[after]));
    }

    #[test]
    fn changing_a_descriptor_changes_the_fingerprint() {
        let before = class("lib/Api", vec![method("compute", "(I)I")]);
        let after = class("lib/Api", vec![method("compute", "(J)I")]);
        assert_ne!(fingerprint(&[before]), fingerprint(&[after]));
    }

    /// The case a signatures-only ABI gets wrong. Both classes have identical member names,
    /// descriptors and flags; only the folded constant differs.
    #[test]
    fn changing_a_constant_changes_the_fingerprint() {
        let before = class("lib/LibKt", vec![constant_field("LIMIT", "Int(30)")]);
        let after = class("lib/LibKt", vec![constant_field("LIMIT", "Int(99)")]);
        assert_eq!(
            before.members[0].descriptor, after.members[0].descriptor,
            "the descriptors are identical — only the constant differs"
        );
        assert_ne!(
            fingerprint(&[before]),
            fingerprint(&[after]),
            "a constant is inlined into consumers, so it must be ABI"
        );
    }

    #[test]
    fn changing_annotation_retention_changes_the_fingerprint() {
        let mut before = class("lib/Marker", vec![]);
        before.retention = Some("SOURCE".into());
        let mut after = class("lib/Marker", vec![]);
        after.retention = Some("RUNTIME".into());
        assert_ne!(fingerprint(&[before]), fingerprint(&[after]));
    }

    #[test]
    fn changing_kotlin_metadata_changes_the_fingerprint() {
        let mut before = class("lib/Api", vec![]);
        before.metadata = "meta-v1".into();
        let mut after = class("lib/Api", vec![]);
        after.metadata = "meta-v2".into();
        assert_ne!(
            fingerprint(&[before]),
            fingerprint(&[after]),
            "@kotlin.Metadata IS the Kotlin ABI on this target"
        );
    }

    #[test]
    fn changing_a_supertype_changes_the_fingerprint() {
        let before = class("lib/Api", vec![]);
        let mut after = class("lib/Api", vec![]);
        after.interfaces = vec!["lib/Marker".into()];
        assert_ne!(fingerprint(&[before]), fingerprint(&[after]));
    }

    #[test]
    fn a_rendering_is_readable_and_carries_no_body() {
        let rendered = class("lib/Api", vec![method("compute", "(I)I")]).render();
        assert!(rendered.contains("class lib/Api"));
        assert!(rendered.contains("method compute (I)I"));
        assert!(
            !rendered.contains("Code"),
            "the ABI model has nowhere to put a body"
        );
    }

    #[test]
    fn fingerprint_displays_as_fixed_width_hex() {
        let rendered = fingerprint(&[class("lib/Api", vec![])]).to_string();
        assert_eq!(rendered.len(), 16);
        assert!(rendered.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn a_malformed_class_file_is_an_error_not_a_panic() {
        assert!(AbiClass::from_class_file(b"not a class file").is_err());
    }
}
