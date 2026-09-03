//! Compact declaration and executable-body contracts emitted by Pass-1 inventory.

use crate::types::Visibility;

use super::identities::{
    BodyOwnerId, DeclarationId, DeclarationKind, LookupNameId, SourceFileId, TextRange,
};
use super::signature::InferredSignatureKind;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BodyKind {
    Function,
    Getter,
    Setter,
    Delegate,
    EnumEntry,
    Constructor,
    Initializer,
    Script,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DeclarationFlags(u64);

impl DeclarationFlags {
    pub const INLINE: u64 = 1 << 0;
    pub const EXTERNAL: u64 = 1 << 1;
    pub const EXPECT: u64 = 1 << 2;
    pub const CONST: u64 = 1 << 3;
    pub const FINAL: u64 = 1 << 4;
    pub const OPEN: u64 = 1 << 5;
    pub const OVERRIDE: u64 = 1 << 6;
    pub const ABSTRACT: u64 = 1 << 7;
    pub const SUSPEND: u64 = 1 << 8;
    pub const TAILREC: u64 = 1 << 9;
    pub const OPERATOR: u64 = 1 << 10;
    pub const INFIX: u64 = 1 << 11;
    pub const MUTABLE: u64 = 1 << 12;
    pub const LATEINIT: u64 = 1 << 13;
    pub const DELEGATED: u64 = 1 << 14;
    pub const EXPLICIT_BACKING_FIELD: u64 = 1 << 15;
    pub const CUSTOM_GETTER: u64 = 1 << 16;
    pub const CUSTOM_SETTER: u64 = 1 << 17;
    pub const INTERFACE: u64 = 1 << 18;
    pub const SINGLETON: u64 = 1 << 19;
    pub const DATA: u64 = 1 << 20;
    pub const VALUE: u64 = 1 << 21;
    pub const ENUM: u64 = 1 << 22;
    pub const FUN_INTERFACE: u64 = 1 << 23;
    pub const SEALED: u64 = 1 << 24;
    pub const ANNOTATION_CLASS: u64 = 1 << 25;
    pub const INNER: u64 = 1 << 26;
    pub const PROPERTY_PARAMETER: u64 = 1 << 27;
    pub const COMPANION: u64 = 1 << 28;
    pub const HAS_INITIALIZER: u64 = 1 << 29;
    pub const LOCAL_CLASS: u64 = 1 << 30;
    /// Declaration synthesized from resolved Kotlin semantics rather than parser syntax. These
    /// declarations still have stable module identities and complete signatures, but never own a
    /// source body or compact header-syntax node.
    pub const COMPILER_GENERATED: u64 = 1 << 31;
    /// Classifier synthesized for one source `object : ... { ... }` expression. The identity is
    /// stable across reparsing; this flag prevents its capture-only constructor parameters from
    /// being mistaken for a second list of source-declared parameters during common lowering.
    pub const ANONYMOUS_OBJECT: u64 = 1 << 32;
    /// A declared getter reads the property's backing-field symbol. This is a stable declaration
    /// shape needed by metadata/storage consumers; they must not inspect the getter body again.
    pub const GETTER_READS_BACKING_FIELD: u64 = 1 << 33;
    /// A setter body was written. This is distinct from `CUSTOM_SETTER`: a visibility-only
    /// declaration such as `private set` has an accessor declaration but still uses default
    /// storage semantics.
    pub const SETTER_HAS_BODY: u64 = 1 << 34;

    pub const fn with(mut self, flag: u64, enabled: bool) -> Self {
        if enabled {
            self.0 |= flag;
        } else {
            self.0 &= !flag;
        }
        self
    }

    pub const fn has(self, flag: u64) -> bool {
        self.0 & flag != 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeclarationStub {
    pub id: DeclarationId,
    pub source: SourceFileId,
    pub range: TextRange,
    /// Temporary source spelling for pass-1 lookup only. Semantic references bind to `id`.
    pub lookup_name: Option<LookupNameId>,
    /// Stable executable-unit classification only. Pass 2 receives the declaration identity and
    /// parser-native stream binding; no source-body locator or parser ID crosses the boundary.
    pub body: Option<BodyKind>,
    pub signature_inference: Option<InferredSignatureKind>,
    /// Exact ordinal in the owning class or enum-entry initialization sequence. This comes from
    /// the parser's semantic `ClassInit` stream while Pass 1 owns the AST; it is not derived from
    /// source coordinates. Only member property declarations and `init` blocks carry a value.
    pub initialization_order: Option<u32>,
    pub kind: DeclarationKind,
    pub visibility: Visibility,
    pub flags: DeclarationFlags,
}

impl DeclarationStub {
    pub const fn body_owner(self) -> BodyOwnerId {
        BodyOwnerId::from_raw(self.id.raw())
    }
}
