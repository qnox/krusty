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
//! * **Declaration annotation payloads.** Non-`SOURCE` annotations can affect a dependent's
//!   frontend decisions, but this reduced model does not yet retain all annotation arguments on
//!   classes, members, parameters, and types.
//! * **Separate public and friend hashes.** Friendship is exact path-set membership, so pruning
//!   `internal` breaks friend modules while keeping it over-invalidates everyone else. Two hashes
//!   per module, with each edge selecting one.
//!
//! [`AbiClass::from_class_file`] cannot prove those omitted facts absent. A concrete compiler
//! adapter must therefore treat this fingerprint as incomplete and key dependents on the full
//! relevant output until a complete semantic ABI model exists.

use crate::digest::{Digest, Hasher};

/// Whether an [`AbiMember`] came from a field or a method.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MemberKind {
    Field,
    Method,
}

/// A JVM `ConstantValue` encoded without text conversion. Floating-point values retain their raw
/// IEEE payload bits and strings retain every UTF-16 code unit, including unpaired surrogates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AbiConstant {
    Int(i32),
    Long(i64),
    FloatBits(u32),
    DoubleBits(u64),
    String(Vec<u16>),
}

/// One lossless class-file annotation payload. Names are serialized boundary identities; values
/// retain their physical tags so (for example) an `Int` cannot collide with a `Boolean`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AbiAnnotation {
    pub name: String,
    pub arguments: Vec<(String, AbiAnnotationValue)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AbiAnnotationValue {
    Int(i32),
    Byte(i8),
    Short(i16),
    Long(i64),
    FloatBits(u32),
    DoubleBits(u64),
    Boolean(bool),
    Char(u16),
    String(Vec<u16>),
    Enum { ty: String, entry: String },
    Class(String),
    Annotation(Box<AbiAnnotation>),
    Array(Vec<AbiAnnotationValue>),
}

impl MemberKind {
    pub(crate) fn tag(self) -> &'static str {
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
    /// A `static final` field's compile-time `ConstantValue`, typed without lossy text conversion.
    /// `None` for methods and non-constant fields. Present because consumers inline constants.
    pub constant: Option<AbiConstant>,
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
    /// `@kotlin.Metadata`, losslessly typed. On the JVM this IS the Kotlin ABI — it is what
    /// `src/jvm/jvm_libraries.rs` reads to type-check a dependent against this class — so it
    /// belongs in the fingerprint rather than beside it.
    pub metadata: Option<AbiAnnotation>,
    /// Members in declaration order. Order is preserved rather than sorted: it is deterministic
    /// (gated by `tests/emission_determinism_e2e.rs`), and preserving it is conservative — a pure
    /// member reordering then invalidates dependents unnecessarily, which costs a rebuild but can
    /// never serve a stale artifact.
    pub members: Vec<AbiMember>,
}

/// A module's ABI condensed to one value. Folded into every dependent's cache key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AbiFingerprint(Digest);

impl AbiFingerprint {
    pub fn digest(self) -> Digest {
        self.0
    }

    /// Rebuild a fingerprint from a previously stored value — used when a cache hit supplies a
    /// module's ABI without recomputing it. Not for minting fingerprints: those come from
    /// [`fingerprint`].
    pub fn from_digest(value: Digest) -> Self {
        Self(value)
    }
}

impl std::fmt::Display for AbiFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
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
                constant: field.const_value.as_ref().map(abi_constant),
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
            metadata: info
                .annotations
                .iter()
                .find(|annotation| annotation.annotation.matches("kotlin/Metadata"))
                .map(abi_annotation),
            members,
        })
    }

    /// Canonical rendering. The fingerprint is a hash of this, so it is also the diff a human reads
    /// when asking why a rebuild triggered.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "class {} access={} super={} interfaces=[{}] signature={:?} retention={:?} meta={:?}\n",
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

    // Length-delimited rather than hashing `render()`: a class name or descriptor containing a
    // newline must not be able to impersonate a second member. See `crate::digest`.
    let mut hasher = Hasher::new();
    hasher.count("classes", ordered.len());
    for class in ordered {
        hasher.text("class", &class.name);
        hasher.field("access", &class.access.to_le_bytes());
        hasher.text("super", class.super_name.as_deref().unwrap_or(""));
        hasher.count("interfaces", class.interfaces.len());
        for interface in &class.interfaces {
            hasher.text("interface", interface);
        }
        hasher.text("signature", class.signature.as_deref().unwrap_or(""));
        hasher.text("retention", class.retention.as_deref().unwrap_or(""));
        match &class.metadata {
            Some(metadata) => {
                hasher.field("has-meta", &[1]);
                absorb_annotation(&mut hasher, metadata);
            }
            None => {
                hasher.field("has-meta", &[0]);
            }
        }
        hasher.count("members", class.members.len());
        for member in &class.members {
            hasher.text("kind", member.kind.tag());
            hasher.text("name", &member.name);
            hasher.text("descriptor", &member.descriptor);
            hasher.field("member-access", &member.access.to_le_bytes());
            hasher.text(
                "member-signature",
                member.signature.as_deref().unwrap_or(""),
            );
            match &member.constant {
                Some(constant) => {
                    hasher.field("has-constant", &[1]);
                    absorb_constant(&mut hasher, constant);
                }
                None => {
                    hasher.field("has-constant", &[0]);
                }
            }
        }
    }
    AbiFingerprint(hasher.finish())
}

fn abi_constant(value: &krusty::jvm::classreader::ConstVal) -> AbiConstant {
    use krusty::jvm::classreader::ConstVal;
    match value {
        ConstVal::Int(value) => AbiConstant::Int(*value),
        ConstVal::Long(value) => AbiConstant::Long(*value),
        ConstVal::Float(value) => AbiConstant::FloatBits(value.to_bits()),
        ConstVal::Double(value) => AbiConstant::DoubleBits(value.to_bits()),
        ConstVal::Str(value) => AbiConstant::String(value.units().collect()),
    }
}

fn abi_annotation(value: &krusty::types::ResolvedAnnotation) -> AbiAnnotation {
    AbiAnnotation {
        name: value.annotation.render(),
        arguments: value
            .arguments
            .iter()
            .map(|(name, value)| (name.clone(), abi_annotation_value(value)))
            .collect(),
    }
}

fn abi_annotation_value(value: &krusty::types::AnnotationValue) -> AbiAnnotationValue {
    use krusty::types::AnnotationValue;
    match value {
        AnnotationValue::Int(value) => AbiAnnotationValue::Int(*value),
        AnnotationValue::Byte(value) => AbiAnnotationValue::Byte(*value),
        AnnotationValue::Short(value) => AbiAnnotationValue::Short(*value),
        AnnotationValue::Long(value) => AbiAnnotationValue::Long(*value),
        AnnotationValue::Float(value) => AbiAnnotationValue::FloatBits(value.to_bits()),
        AnnotationValue::Double(value) => AbiAnnotationValue::DoubleBits(value.to_bits()),
        AnnotationValue::Boolean(value) => AbiAnnotationValue::Boolean(*value),
        AnnotationValue::Char(value) => AbiAnnotationValue::Char(*value),
        AnnotationValue::String(value) => AbiAnnotationValue::String(value.units().collect()),
        AnnotationValue::Enum(ty, entry) => AbiAnnotationValue::Enum {
            ty: ty.render(),
            entry: entry.clone(),
        },
        AnnotationValue::Class(ty) => AbiAnnotationValue::Class(ty.render()),
        AnnotationValue::Annotation { internal, values } => {
            AbiAnnotationValue::Annotation(Box::new(AbiAnnotation {
                name: internal.render(),
                arguments: values
                    .iter()
                    .map(|(name, value)| (name.clone(), abi_annotation_value(value)))
                    .collect(),
            }))
        }
        AnnotationValue::Array(values) => {
            AbiAnnotationValue::Array(values.iter().map(abi_annotation_value).collect())
        }
    }
}

fn absorb_constant(hasher: &mut Hasher, value: &AbiConstant) {
    match value {
        AbiConstant::Int(value) => {
            hasher.text("constant-kind", "int");
            hasher.field("constant-value", &value.to_le_bytes());
        }
        AbiConstant::Long(value) => {
            hasher.text("constant-kind", "long");
            hasher.field("constant-value", &value.to_le_bytes());
        }
        AbiConstant::FloatBits(value) => {
            hasher.text("constant-kind", "float");
            hasher.field("constant-value", &value.to_le_bytes());
        }
        AbiConstant::DoubleBits(value) => {
            hasher.text("constant-kind", "double");
            hasher.field("constant-value", &value.to_le_bytes());
        }
        AbiConstant::String(units) => {
            hasher.text("constant-kind", "string");
            hasher.count("constant-units", units.len());
            for unit in units {
                hasher.field("constant-unit", &unit.to_le_bytes());
            }
        }
    }
}

fn absorb_annotation(hasher: &mut Hasher, annotation: &AbiAnnotation) {
    hasher.text("annotation", &annotation.name);
    hasher.count("annotation-arguments", annotation.arguments.len());
    for (name, value) in &annotation.arguments {
        hasher.text("annotation-argument-name", name);
        absorb_annotation_value(hasher, value);
    }
}

fn absorb_annotation_value(hasher: &mut Hasher, value: &AbiAnnotationValue) {
    match value {
        AbiAnnotationValue::Int(value) => typed_value(hasher, "int", &value.to_le_bytes()),
        AbiAnnotationValue::Byte(value) => typed_value(hasher, "byte", &value.to_le_bytes()),
        AbiAnnotationValue::Short(value) => typed_value(hasher, "short", &value.to_le_bytes()),
        AbiAnnotationValue::Long(value) => typed_value(hasher, "long", &value.to_le_bytes()),
        AbiAnnotationValue::FloatBits(value) => typed_value(hasher, "float", &value.to_le_bytes()),
        AbiAnnotationValue::DoubleBits(value) => {
            typed_value(hasher, "double", &value.to_le_bytes())
        }
        AbiAnnotationValue::Boolean(value) => typed_value(hasher, "boolean", &[*value as u8]),
        AbiAnnotationValue::Char(value) => typed_value(hasher, "char", &value.to_le_bytes()),
        AbiAnnotationValue::String(units) => {
            hasher.text("annotation-value-kind", "string");
            hasher.count("annotation-string-units", units.len());
            for unit in units {
                hasher.field("annotation-string-unit", &unit.to_le_bytes());
            }
        }
        AbiAnnotationValue::Enum { ty, entry } => {
            hasher.text("annotation-value-kind", "enum");
            hasher.text("annotation-enum-type", ty);
            hasher.text("annotation-enum-entry", entry);
        }
        AbiAnnotationValue::Class(ty) => {
            hasher.text("annotation-value-kind", "class");
            hasher.text("annotation-class", ty);
        }
        AbiAnnotationValue::Annotation(annotation) => {
            hasher.text("annotation-value-kind", "annotation");
            absorb_annotation(hasher, annotation);
        }
        AbiAnnotationValue::Array(values) => {
            hasher.text("annotation-value-kind", "array");
            hasher.count("annotation-array-values", values.len());
            for value in values {
                absorb_annotation_value(hasher, value);
            }
        }
    }
}

fn typed_value(hasher: &mut Hasher, kind: &str, bytes: &[u8]) {
    hasher.text("annotation-value-kind", kind);
    hasher.field("annotation-value", bytes);
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

    fn constant_field(name: &str, value: AbiConstant) -> AbiMember {
        AbiMember {
            kind: MemberKind::Field,
            name: name.into(),
            descriptor: "I".into(),
            access: 25,
            signature: None,
            constant: Some(value),
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
            metadata: Some(AbiAnnotation {
                name: "kotlin/Metadata".into(),
                arguments: vec![("k".into(), AbiAnnotationValue::Int(1))],
            }),
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
        let before = class(
            "lib/LibKt",
            vec![constant_field("LIMIT", AbiConstant::Int(30))],
        );
        let after = class(
            "lib/LibKt",
            vec![constant_field("LIMIT", AbiConstant::Int(99))],
        );
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
    fn distinct_nan_payload_bits_never_collapse_through_debug_text() {
        let first = abi_constant(&krusty::jvm::classreader::ConstVal::Float(f32::from_bits(
            0x7fc0_0001,
        )));
        let second = abi_constant(&krusty::jvm::classreader::ConstVal::Float(f32::from_bits(
            0x7fc0_0002,
        )));
        assert_eq!(first, AbiConstant::FloatBits(0x7fc0_0001));
        assert_eq!(second, AbiConstant::FloatBits(0x7fc0_0002));

        let before = class("lib/LibKt", vec![constant_field("NAN", first)]);
        let after = class("lib/LibKt", vec![constant_field("NAN", second)]);
        assert_ne!(fingerprint(&[before]), fingerprint(&[after]));
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
        before.metadata = Some(AbiAnnotation {
            name: "kotlin/Metadata".into(),
            arguments: vec![(
                "d1".into(),
                AbiAnnotationValue::Array(vec![AbiAnnotationValue::String(vec![1])]),
            )],
        });
        let mut after = class("lib/Api", vec![]);
        after.metadata = Some(AbiAnnotation {
            name: "kotlin/Metadata".into(),
            arguments: vec![(
                "d1".into(),
                AbiAnnotationValue::Array(vec![AbiAnnotationValue::String(vec![2])]),
            )],
        });
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
        assert_eq!(rendered.len(), 64, "SHA-256 renders as 64 hex characters");
        assert!(rendered.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn a_malformed_class_file_is_an_error_not_a_panic() {
        assert!(AbiClass::from_class_file(b"not a class file").is_err());
    }
}
