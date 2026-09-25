//! The constructors a class declares beyond its primary one, and the constructor a delegation
//! reaches.
//!
//! A secondary constructor is neither a function nor a property: it carries its own parameters,
//! defaults, delegation and declaration line, so every phase that needs one of those reads this
//! declaration rather than reconstructing it from the primary or from a body expression.

use super::{CtorDelegateTarget, DeclarationAnnotations, ExprId, IrGeneratedDeclarationDebug, Ty};

/// The checker-selected constructor a `super(…)`/`this(…)` delegation reaches, as the declaration
/// facts a target's constructor ABI depends on. Lowering records them once from the selection; a
/// backend never recovers them from the target class's IR, which another file may own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IrConstructorTarget {
    /// The selected constructor is its class's primary declaration.
    pub primary: bool,
    pub access: IrConstructorAccess,
}

impl IrConstructorTarget {
    /// The primary constructor of an ordinary class, as a compiler-synthesized class reaches its
    /// superclass (`Any`, a lambda or continuation base, an enum base).
    pub const UNRESTRICTED_PRIMARY: Self = Self {
        primary: true,
        access: IrConstructorAccess::Unrestricted,
    };
}

/// Who Kotlin lets call a selected constructor, beyond the call already being well-typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrConstructorAccess {
    /// Any caller that selected it: a public, protected or internal constructor of a class that is
    /// not sealed, or a compiler-generated public one.
    Unrestricted,
    /// A `private` constructor of a class that is not sealed: only its own class.
    Private,
    /// A declared constructor of a `sealed` class, which is always `protected` or `private`: only
    /// the class and its subclasses.
    SealedClass,
}

/// A secondary constructor: `<init>(params)` runs `delegate_prelude`, loads `delegate_args`, calls the
/// delegate target, then runs `body`. `this` is value 0 and parameters are values `1..=params.len()`.
#[derive(Clone, Debug)]
pub struct IrSecondaryCtor {
    /// User annotations declared on this constructor, split by JVM retention — the constructor
    /// analogue of [`IrFile::function_annotations`] (a secondary constructor is not an
    /// [`IrFunction`], so it carries them directly).
    pub annotations: DeclarationAnnotations,
    /// Stable source byte offset of a declared constructor. Generated constructors use
    /// `u32::MAX`; their producer records any later placement rule by exact identity.
    pub source_order: u32,
    /// The source lines this constructor's declaration owns. A backend must never recover one of
    /// them from whichever descendant expression happens to carry provenance: a default
    /// initializer, a delegation and the declaration itself are different source facts on
    /// different lines.
    pub lines: IrSecondaryCtorLines,
    /// Compiler-supplied leading parameters shared by every constructor of the class. These occupy
    /// body value slots before `params`, but are absent from Kotlin source metadata and default masks.
    pub prefix_params: Vec<Ty>,
    pub params: Vec<Ty>,
    /// SOURCE parameter names paired with SEMANTIC (checker-resolved) types — what the class
    /// `@Metadata` `Constructor` record describes (`params` above are the erased IR realization,
    /// which loses fun-type shapes and generic arguments). This is metadata payload, not a
    /// publication sentinel: [`Self::metadata_visibility`] alone decides whether a record exists.
    pub named_params: Vec<(String, Ty)>,
    /// Publish this constructor in Kotlin metadata with the recorded semantic visibility. `None`
    /// means the constructor is a target/compiler realization with no Kotlin declaration record,
    /// independently of its parameter names, arity, descriptor, or [`Self::synthetic`] flag.
    pub metadata_visibility: Option<crate::types::Visibility>,
    /// Debug representation for a compiler-generated constructor. Source constructors derive their
    /// own tables from source declarations and leave this as `None`.
    pub generated_debug: IrGeneratedDeclarationDebug,
    /// Index into `named_params` of a `vararg` parameter, for the `Constructor` metadata record.
    pub vararg_index: Option<usize>,
    pub defaults: Vec<Option<ExprId>>,
    /// Source-ordered temp declarations for delegation arguments.
    pub delegate_prelude: Vec<ExprId>,
    pub delegate_args: Vec<ExprId>,
    /// Semantic target-parameter ordinals omitted at this delegation site. A backend derives its
    /// own default-constructor ABI (for example JVM masks and marker) from these checked ordinals.
    pub default_parameters: Vec<u32>,
    pub body: Option<ExprId>,
    /// Which `<init>` this constructor delegates to, and whether it runs the class init body.
    pub delegate: CtorDelegateTarget,
    /// kotlinc marks this ctor `ACC_SYNTHETIC` (0x1000) — e.g. a `@Serializable` deserialization ctor.
    pub synthetic: bool,
    /// A DECLARED parameter was value-class-typed (recorded by the value-class pass before erasure):
    /// the ctor gets kotlinc's PRIVATE + public synthetic `(…, DefaultConstructorMarker)` ABI, and
    /// its metadata record names the marker form.
    pub vc_params: bool,
}

/// The source lines a secondary constructor's DECLARATION owns, each recorded where the syntax was
/// live. All are 1-based; 0 means unknown, which is every generated constructor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IrSecondaryCtorLines {
    /// The `constructor` keyword — where the synthetic default overload enters.
    pub decl_line: u32,
    /// The `this`/`super` keyword this constructor delegates through.
    pub delegation_line: u32,
    /// The declaration's last line — its delegation's closing `)` or its block's `}`.
    pub decl_end_line: u32,
    /// Each parameter's default expression, parallel to the DECLARED parameters; 0 = no default.
    pub defaults: Vec<u32>,
}

/// A compiler-generated secondary constructor's semantic role. Producers record this exact class
/// and ordinal edge once; later plugin/backend phases must not recover the constructor from
/// `synthetic`, its parameter arity, descriptor, or generated spelling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum IrSecondaryConstructorRole {
    SerializationDeserialization,
}

/// Semantic declaration metadata retained when the JVM value-class pass replaces a secondary
/// constructor with a static `constructor-impl` realization. The backend owns the physical handle;
/// Kotlin metadata must still describe the original source parameters/defaults and link them to that
/// exact handle for downstream frontend resolution.
#[derive(Clone, Debug)]
pub struct IrJvmValueClassSecondaryCtor {
    pub params: Vec<(String, Ty)>,
    pub param_defaults: Vec<bool>,
    pub vararg_index: Option<usize>,
    pub annotations: DeclarationAnnotations,
    pub metadata_visibility: crate::types::Visibility,
    pub descriptor: String,
}
