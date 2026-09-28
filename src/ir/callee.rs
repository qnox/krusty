//! The target of a call: which declaration an `IrExpr::Call` invokes and how it is dispatched.

use crate::libraries::InlineKind;
use crate::types::{Ty, TypeName};

use super::{FunId, IrCheckedSubstitution, IrIntrinsic};

/// The target of an `IrExpr::Call`. `Local` references a function defined in this IR file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Callee {
    Local(FunId),
    /// A static method defined on a class in this IR file. Unlike [`Callee::Local`], whose owner is
    /// the file facade, this carries the declaring class explicitly. The `FunId` keeps the call tied
    /// to the function's semantic parameter/return types through backend ABI transformations.
    ClassStatic {
        owner: TypeName,
        function: FunId,
    },
    /// A checked call to a static source function with omitted semantic parameters. The argument
    /// vector contains only supplied values in declaration order; defaults names omitted ordinals.
    /// A backend chooses its own default-argument calling convention.
    ClassStaticWithDefaults {
        owner: TypeName,
        function: FunId,
        defaults: Box<[u32]>,
    },
    /// `$default` companion of a static function owned by a class in this IR file.
    ClassStaticDefault {
        owner: TypeName,
        function: FunId,
    },
    /// The `$default` synthetic of a same-file top-level function/extension (`FunId`) — emitted as
    /// `invokestatic <facade>.<name>$default(realparams, int mask, Object marker)ret`. Like `Local` the
    /// facade is resolved at emit (`self.facade`); the descriptor appends the trailing `I` mask +
    /// `Object` marker to the real function's parameters. Used when a call omits a (possibly non-const)
    /// defaulted argument, mirroring kotlinc's default-argument ABI.
    LocalDefault(FunId),
    /// A checked call to a source function in this IR file with omitted semantic parameters.
    /// The argument vector contains only supplied values in declaration order.
    LocalWithDefaults {
        function: FunId,
        defaults: Box<[u32]>,
    },
    Intrinsic {
        operation: IrIntrinsic,
        ret: Ty,
    },
    /// A top-level function defined in ANOTHER source file of the same multi-file compilation —
    /// `invokestatic <facade>.<name>(params)ret`. Carries the signature as backend-agnostic `Ty`s
    /// (the JVM backend builds the descriptor), so common lowering never needs JVM descriptors. Distinct
    /// from `Local` (same IrFile, by index) and `Static` (a resolved classpath/library method).
    CrossFile {
        facade: TypeName,
        name: String,
        params: Vec<Ty>,
        ret: Ty,
        /// Exact current-module declaration that produced this realized cross-file edge.
        module_target: Option<crate::fir::CallableId>,
        /// Whether this edge invokes the declaration's default-argument synthetic. Kept separate
        /// from the JVM spelling so representation passes never infer semantics from `$default`.
        module_default_call: bool,
    },
    /// A top-level callable in another source unit of the current module. The stable target is
    /// backend-neutral; a module realization pass maps its declaring [`SourceFileId`](crate::fir::SourceFileId)
    /// to the target's physical file container without repeating semantic selection.
    Module {
        target: crate::fir::CallableId,
        name: String,
        params: Vec<Ty>,
        ret: Ty,
    },
    /// A checked same-module call with omitted semantic parameters. Unlike Module, this form
    /// contains no target ABI masks, marker parameter, synthetic name, or placeholder values.
    /// The argument vector contains supplied values in declaration order; defaults identifies holes.
    ModuleWithDefaults {
        target: crate::fir::CallableId,
        default_provider: crate::fir::ResolvedFunctionOverrideTarget,
        name: String,
        params: Vec<Ty>,
        ret: Ty,
        defaults: Box<[u32]>,
        /// Semantic dispatch-receiver type for a member default call. It remains separate from the
        /// declared parameter list and is transformed before a backend places that receiver in its
        /// physical default-call convention.
        dispatch_receiver_ty: Option<Ty>,
        /// Checked position of an extension receiver in params. A backend that numbers default
        /// masks independently from the receiver consumes this fact directly.
        extension_receiver_parameter: Option<u32>,
    },
    /// A dependency callable selected by the frontend. The opaque declaration identity is realized
    /// by the target provider after common lowering; semantic parameters/results remain available to
    /// backend-neutral passes without exposing an owner or descriptor.
    External {
        target: crate::fir::ExternalCallableId,
        /// Dependency declaration that supplies inherited defaults for this selected target.
        /// Present only as an opaque checked identity; a backend owns its physical realization.
        default_provider: Option<crate::fir::ExternalCallableId>,
        params: Vec<Ty>,
        ret: Ty,
        /// Final checked type substitutions, keyed by the provider-owned declaration parameter.
        /// A target backend may translate the stable ordinal to its physical metadata name; no
        /// common-IR consumer performs lookup or inference from these values.
        substitutions: Vec<IrCheckedSubstitution>,
        /// Final semantic parameter ordinals omitted at the source call site. A target backend uses
        /// its provider-owned default bridge; common lowering never reconstructs that ABI.
        defaults: Vec<u32>,
        /// Checked position of a member-extension receiver in `params`/`args`. Default-mask
        /// ordinals exclude this receiver; target realization consumes this exact semantic fact
        /// instead of inferring source shape from a physical provider descriptor.
        extension_receiver_parameter: Option<u32>,
    },
    /// A resolved classpath static method — `invokestatic owner.name:descriptor`. Used for stdlib
    /// extension/top-level functions resolved from the classpath (`StringsKt.repeat`, `RangesKt.until`),
    /// carrying the exact JVM descriptor so no name is hardcoded in the backend.
    /// `inline` carries the callee's inline-ness in one field (was `inline` + `must_inline`):
    /// [`InlineKind::CanInline`] => a Kotlin `inline` function whose compiled body the JVM backend may
    /// splice here instead of emitting the `invokestatic`; [`InlineKind::MustInline`] => a NON-PUBLIC
    /// `@InlineOnly` callee (`require`/`check`/`error`) with no legal `invokestatic` fallback, so the
    /// backend MUST splice the body (a body it can't splice — e.g. branchy on a non-empty operand stack —
    /// skips the whole file, never miscompiled).
    Static {
        owner: TypeName,
        name: String,
        descriptor: String,
        inline: InlineKind,
    },
    /// An instance method (or property accessor) — `invokevirtual`/`invokeinterface owner.name(sig)` on
    /// the `dispatch_receiver`; `interface` ⇒ `invokeinterface`. `owner` is the receiver's static type.
    /// The SOLE virtual-dispatch callee (classpath, same-file, and sibling-file all unified — no
    /// cp/module/local split). The descriptor comes from ONE source, like [`IrExpr::New`]:
    /// - `params: Some((param_tys, ret))` — a user method (typically sibling-file) whose descriptor the
    ///   JVM backend builds from `Ty`s, and whose value-class name-mangle/erasure the pass applies.
    /// - else `descriptor` — a verbatim JVM descriptor (classpath, or an already-resolved same-file form).
    Virtual {
        owner: TypeName,
        name: String,
        descriptor: String,
        params: Option<(Vec<Ty>, Ty)>,
        interface: bool,
        /// Exact current-module declaration the checker selected, when the call names one.
        module_target: Option<crate::fir::CallableId>,
    },
    /// A non-virtual instance call — `invokespecial owner.name:descriptor` on the `dispatch_receiver`.
    /// Used for `super.method(…)`, which dispatches through the source-level super qualifier directly
    /// (skipping the receiver's override). `owner` is that qualifier unless JVM realization must name
    /// a class declaration reached through an interface qualifier.
    /// A `super`-qualified call before target realization: the checker fixed one supertype
    /// declaration and dispatch is non-virtual, but the physical descriptor and whether the body
    /// lives in a JVM-default holder are target choices. `jvm::module_calls` realizes this into
    /// [`Callee::Special`].
    Super {
        owner: TypeName,
        /// Exact classifier whose instance supplies the nonvirtual dispatch receiver.
        dispatch_owner: TypeName,
        /// The call appears in a different lexical classifier and therefore needs a target-specific
        /// owner bridge; emitting `invokespecial` directly from the inner class is verifier-invalid.
        enclosing_dispatch: bool,
        kind: IrSuperCallKind,
        name: String,
        params: Vec<Ty>,
        ret: Ty,
        interface: bool,
        /// Exact provider realization selected before common lowering. Targets consume this opaque
        /// fact; they do not rediscover a holder/static shape from owner spellings.
        realization: crate::libraries::MemberRealization,
        /// Provider-owned physical descriptor when one exists; source declarations leave it empty
        /// and the target backend derives its ABI from `params`/`ret`.
        descriptor: String,
        /// Exact source callable owning checked default expressions, when this super declaration is
        /// part of the current module.
        source: Option<crate::fir::CallableId>,
        /// Exact dependency declaration selected for this super call. A target resolves its own
        /// physical realization of that declaration (a legacy interface body's holder) from it.
        external: Option<crate::fir::ExternalCallableId>,
        /// Final semantic parameter ordinals omitted at the checked call site.
        defaults: Vec<u32>,
        source_member: Option<crate::libraries::SourceMember>,
    },
    Special {
        owner: TypeName,
        name: String,
        descriptor: String,
        /// `owner` is an INTERFACE (a diamond `super.f()` dispatched to a superinterface's DEFAULT method):
        /// the method reference must be an `InterfaceMethodref` and the call an `invokespecial` on it.
        interface: bool,
        /// Exact source declaration selected for a current-compilation interface body. A JVM backend
        /// may relocate that body according to the requested output mode; dependency realizations
        /// arrive as `Callee::Static` instead and leave this unset.
        source_member: Option<crate::libraries::SourceMember>,
        /// Stable current-module callable when this special call realizes a source declaration.
        /// This remains present in compact-header Pass 2 even when the legacy source-member
        /// coordinate is deliberately absent.
        source: Option<crate::fir::CallableId>,
    },
}

/// Source-level member operation selected for a semantic `super` dispatch. This common-IR identity
/// is target-neutral; backends realize the getter/setter spelling and physical invocation shape.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrSuperCallKind {
    Function,
    PropertyGetter,
    PropertySetter,
}

impl Callee {
    /// The function declaration stored in this IR file that owns this call's semantic signature.
    ///
    /// Default-dispatch calls still point at the source function; only their emitted entry point is
    /// synthetic. Keeping that classification here prevents every IR consumer from maintaining its
    /// own list of local/static and ordinary/default variants.
    pub(crate) fn source_function(&self) -> Option<FunId> {
        match self {
            Callee::Local(function)
            | Callee::LocalWithDefaults { function, .. }
            | Callee::LocalDefault(function)
            | Callee::ClassStatic { function, .. }
            | Callee::ClassStaticWithDefaults { function, .. }
            | Callee::ClassStaticDefault { function, .. } => Some(*function),
            _ => None,
        }
    }
}
