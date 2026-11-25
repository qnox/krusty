use super::*;

/// Bit-packed boolean flags for a [`Signature`], collapsing `vararg`/`is_inline`/`is_operator`/
/// `is_override`/`is_final`/`is_suspend`/`requires_splice`/`has_reified_type_params`/`is_abstract`/
/// `low_priority` into one word. Read through the
/// `Signature` accessors of the same names; `vararg` is also mutated through `set_vararg`; built with
/// the `with_*` chain.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SigFlags(u16);

impl SigFlags {
    const VARARG: u16 = 1 << 0;
    const IS_INLINE: u16 = 1 << 1;
    const IS_OPERATOR: u16 = 1 << 2;
    const IS_OVERRIDE: u16 = 1 << 3;
    const IS_FINAL: u16 = 1 << 4;
    const IS_SUSPEND: u16 = 1 << 5;
    const REQUIRES_SPLICE: u16 = 1 << 6;
    const IS_ABSTRACT: u16 = 1 << 7;
    const IS_INFIX: u16 = 1 << 8;
    const HAS_REIFIED_TYPE_PARAMS: u16 = 1 << 9;
    const IS_COMPANION_EXTENSION: u16 = 1 << 10;
    const IS_COMPANION_BLOCK_MEMBER: u16 = 1 << 11;

    #[inline]
    const fn with(mut self, mask: u16, on: bool) -> Self {
        if on {
            self.0 |= mask;
        } else {
            self.0 &= !mask;
        }
        self
    }
    #[inline]
    const fn has(self, mask: u16) -> bool {
        self.0 & mask != 0
    }

    #[inline]
    pub const fn with_vararg(self, on: bool) -> Self {
        self.with(Self::VARARG, on)
    }
    #[inline]
    pub const fn with_is_inline(self, on: bool) -> Self {
        self.with(Self::IS_INLINE, on)
    }
    #[inline]
    pub const fn with_is_operator(self, on: bool) -> Self {
        self.with(Self::IS_OPERATOR, on)
    }
    #[inline]
    pub const fn with_is_override(self, on: bool) -> Self {
        self.with(Self::IS_OVERRIDE, on)
    }
    #[inline]
    pub const fn with_is_final(self, on: bool) -> Self {
        self.with(Self::IS_FINAL, on)
    }
    #[inline]
    pub const fn with_is_suspend(self, on: bool) -> Self {
        self.with(Self::IS_SUSPEND, on)
    }
    #[inline]
    pub const fn with_requires_splice(self, on: bool) -> Self {
        self.with(Self::REQUIRES_SPLICE, on)
    }
    #[inline]
    pub const fn with_has_reified_type_params(self, on: bool) -> Self {
        self.with(Self::HAS_REIFIED_TYPE_PARAMS, on)
    }
    #[inline]
    pub const fn with_is_abstract(self, on: bool) -> Self {
        self.with(Self::IS_ABSTRACT, on)
    }
    #[inline]
    pub const fn with_is_infix(self, on: bool) -> Self {
        self.with(Self::IS_INFIX, on)
    }
    #[inline]
    pub const fn with_is_companion_extension(self, on: bool) -> Self {
        self.with(Self::IS_COMPANION_EXTENSION, on)
    }
    #[inline]
    pub const fn with_is_companion_block_member(self, on: bool) -> Self {
        self.with(Self::IS_COMPANION_BLOCK_MEMBER, on)
    }
}

#[derive(Clone, Debug)]
pub struct Signature {
    pub params: Vec<Ty>,
    pub ret: Ty,
    /// Declared generic callable shape retained for call-site inference.
    pub generic_sig: Option<GenericSig>,
    pub projected_return_hazard: bool,
    /// Bit-packed `vararg`/`is_inline`/`is_operator`/`is_override`/`is_final`/`is_suspend`/
    /// `requires_splice` (read via the accessors below; `vararg` set via `set_vararg`). `vararg` marks
    /// a variadic signature. `is_final` — a `final` member a subclass cannot override. `is_suspend`
    /// — a `suspend fun`. `requires_splice` — no direct-call fallback is semantically legal even when
    /// the backend emits a method so another compilation can obtain and inline its body.
    pub flags: SigFlags,
    /// Annotation class identities declared on the source declaration. Consumers decide which
    /// annotations affect resolution/emission; the signature layer only records their identities.
    pub annotations: Vec<crate::types::TypeName>,
    /// Classifier promised by `@EqualityBound(C::class)` on the `equals(Any?)` parameter. The
    /// physical parameter remains `Any?`; this compact semantic fact drives strict-equality flow.
    pub equality_bound: Option<Ty>,
    pub vararg_index: Option<usize>,
    /// Minimum number of arguments a caller must supply — params beyond this have default values
    /// that the caller fills in. Equals `params.len()` when there are no defaults.
    pub required: usize,
    /// For each parameter: whether it has a default value (so it may be omitted by name or position).
    /// Parallel to `params`; empty when unknown (callers then fall back to the `required` prefix count).
    /// This captures a *non-trailing* default (`f(x: Int = 3, g: () -> Int)`) that `required` cannot.
    pub param_defaults: crate::libraries::ParamFlags,
    /// Per parameter: its declared type carries Kotlin's internal `@Exact` constraint.
    pub exact_params: crate::libraries::ParamFlags,
    /// Per parameter: its declared type carries Kotlin's internal `@NoInfer` constraint.
    pub no_infer_params: crate::libraries::ParamFlags,
    /// Per parameter, the resolved declaration carries
    /// `kotlin.internal.ImplicitIntegerCoercion`.
    pub implicit_integer_coercion: crate::libraries::ParamFlags,
    /// Parameter names, parallel to `params`. Used to map named arguments (`f(x = 1)`) to positions.
    /// Empty for signatures where named-argument calls aren't supported (methods, synthesized members).
    pub param_names: Vec<String>,
    /// Provider-normalized semantic identities parallel to `params`. Source context roles are
    /// captured from compact headers instead of being reconstructed from their spelling later.
    pub parameter_identities: Vec<crate::fir::ResolvedParameterIdentity>,
    /// File-independent default values, parallel to `params`. `Some` only for literal/object defaults the
    /// lowerer can emit without dereferencing the declaring file's AST arena.
    pub param_default_values: Vec<Option<CtorDefaultValue>>,
    /// For each parameter: if the parameter is a function type `(A, B) -> R`, the inner parameter
    /// types `[A, B]`; otherwise an empty Vec. Used to type-check lambda arguments with the correct
    /// `it` / parameter types. Parallel to `params`.
    pub lambda_param_types: Vec<Vec<Ty>>,
    /// For each parameter: `true` when it is a RECEIVER function type `Recv.(A) -> R` (vs a plain
    /// `(Recv, A) -> R`). A lambda passed to such a param binds `lambda_param_types[i][0]` as the
    /// implicit `this` receiver (so a bare member/extension call inside resolves against it). Parallel
    /// to `params`; empty = none are receivers.
    pub lambda_recv: crate::libraries::ParamFlags,
    /// Per logical parameter, the `crossinline`/`noinline` modifier it wrote. Parallel to `params`;
    /// empty when the source publishes none.
    pub inline_modifiers: Vec<crate::types::InlineParameterModifier>,
    /// Source visibility.
    pub visibility: Visibility,
    /// Number of leading context parameters in `params`. Ordinary functions leave this at 0.
    pub context_count: usize,
    /// Source declaration id for a top-level function in the current compilation. Member, local, and
    /// classpath signatures leave this unset.
    pub source_decl: Option<DeclId>,
    /// Stable frontend identity for this source declaration. Unlike `source_decl`, this survives
    /// body reparsing and is the only identity copied into checked FIR.
    pub stable_declaration: Option<crate::fir::DeclarationId>,
    /// Source file index for an AST-backed top-level or member signature.
    pub source_file: Option<u32>,
    /// Exact AST coordinate of a selected member declaration. Top-level and synthesized signatures
    /// leave it unset; lowering follows this coordinate without scanning names, owners, or spans.
    pub source_member: Option<crate::libraries::SourceMember>,
    /// Declared extension receiver before lookup-key erasure.
    pub source_receiver: Option<Ty>,
    /// Declaring package of a source top-level declaration. The root is the default package.
    pub package: TypeName,
    /// The function's decoded `contract { … }`, when it declares one. Filled by the checker
    /// (which confirms the intrinsic identity) after checking, so cross-file call sites and the
    /// `@Metadata` emitter see the same effects the checker's call-site application uses.
    pub contract: Option<std::sync::Arc<crate::contracts::Contract>>,
    /// Plugin-owned implementation for a compiler-contributed declaration. This is declaration
    /// identity carried through ordinary overload selection, never a source-name convention.
    pub plugin_expression: Option<crate::libraries::PluginExpressionDeclaration>,
}

impl Signature {
    #[inline]
    pub fn vararg(&self) -> bool {
        self.flags.has(SigFlags::VARARG)
    }
    #[inline]
    pub fn is_inline(&self) -> bool {
        self.flags.has(SigFlags::IS_INLINE)
    }
    /// Whether a lambda argument for logical parameter `parameter` is inlined into the caller's
    /// frame: the callable is inline and the parameter wrote neither `crossinline` nor `noinline`.
    pub fn inlines_lambda_for(&self, parameter: usize) -> bool {
        self.is_inline()
            && crate::types::InlineParameterModifier::runs_parameter_in_caller_frame(
                &self.inline_modifiers,
                parameter,
            )
    }
    #[inline]
    pub fn is_operator(&self) -> bool {
        self.flags.has(SigFlags::IS_OPERATOR)
    }
    #[inline]
    pub fn is_infix(&self) -> bool {
        self.flags.has(SigFlags::IS_INFIX)
    }
    #[inline]
    pub fn is_override(&self) -> bool {
        self.flags.has(SigFlags::IS_OVERRIDE)
    }
    #[inline]
    pub fn is_final(&self) -> bool {
        self.flags.has(SigFlags::IS_FINAL)
    }
    #[inline]
    pub fn is_suspend(&self) -> bool {
        self.flags.has(SigFlags::IS_SUSPEND)
    }
    #[inline]
    pub fn requires_splice(&self) -> bool {
        self.flags.has(SigFlags::REQUIRES_SPLICE)
    }
    #[inline]
    pub fn has_reified_type_params(&self) -> bool {
        self.flags.has(SigFlags::HAS_REIFIED_TYPE_PARAMS)
    }
    #[inline]
    pub fn is_abstract(&self) -> bool {
        self.flags.has(SigFlags::IS_ABSTRACT)
    }
    #[inline]
    pub fn is_companion_extension(&self) -> bool {
        self.flags.has(SigFlags::IS_COMPANION_EXTENSION)
    }
    #[inline]
    pub fn is_companion_block_member(&self) -> bool {
        self.flags.has(SigFlags::IS_COMPANION_BLOCK_MEMBER)
    }
    #[inline]
    pub fn set_vararg(&mut self, on: bool) {
        self.flags = self.flags.with_vararg(on);
        if !on {
            self.vararg_index = None;
        }
    }
    #[inline]
    pub fn set_is_operator(&mut self, on: bool) {
        self.flags = self.flags.with_is_operator(on);
    }
    #[inline]
    pub fn set_is_inline(&mut self, on: bool) {
        self.flags = self.flags.with_is_inline(on);
    }

    pub fn single_param(&self) -> Option<Ty> {
        (self.params.len() == 1).then(|| self.params[0])
    }

    pub fn call_sig(&self) -> CallSig {
        let mut call_sig = CallSig::source(
            self.param_names.clone(),
            self.param_defaults,
            self.lambda_param_types.clone(),
            self.lambda_recv,
            self.params
                .iter()
                .map(|parameter| match parameter.kind() {
                    TyKind::Fun(function) => function.context_count,
                    _ => 0,
                })
                .collect(),
            self.required,
            self.vararg_index,
        );
        call_sig.exact_params = self.exact_params;
        call_sig.no_infer_params = self.no_infer_params;
        call_sig.implicit_integer_coercion = self.implicit_integer_coercion;
        call_sig.inline_modifiers = self.inline_modifiers.clone();
        copy_parameter_identities(&mut call_sig, &self.parameter_identities, self.params.len());
        call_sig
    }
}

/// Source extension overloads applicable to one receiver.
pub struct ExtensionOverloads<'symbols, 'scope> {
    pub(super) exact: Option<&'symbols [Signature]>,
    pub(super) ranked: Vec<(u32, &'symbols Signature)>,
    pub(super) packages: Option<&'scope [TypeName]>,
    pub(super) import_scope: Option<&'scope crate::symbol_resolver::FunctionImportScope>,
    pub(super) scope_name: Option<&'scope str>,
}

impl<'symbols, 'scope> ExtensionOverloads<'symbols, 'scope> {
    pub fn iter(&self) -> impl Iterator<Item = &'symbols Signature> + '_ {
        let packages = self.packages;
        let import_scope = self.import_scope;
        let scope_name = self.scope_name;
        let explicit_packages = import_scope
            .zip(scope_name)
            .map(|(scope, name)| {
                scope
                    .explicit_targets(name)
                    .into_iter()
                    .filter_map(|(owner, _)| match owner {
                        crate::symbol_source::SymbolNamespace::Package(package) => Some(package),
                        crate::symbol_source::SymbolNamespace::Classifier(_) => None,
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let selected_import_level = import_scope.and_then(|scope| {
            let name = scope_name?;
            if scope.has_explicit(name) {
                return None;
            }
            scope.levels().iter().position(|level| {
                self.exact
                    .unwrap_or(&[])
                    .iter()
                    .chain(self.ranked.iter().map(|(_, signature)| *signature))
                    .any(|signature| level.contains(&signature.package))
            })
        });
        self.exact
            .unwrap_or(&[])
            .iter()
            .chain(self.ranked.iter().map(|(_, signature)| *signature))
            .filter(move |signature| {
                import_scope.is_some_and(|scope| {
                    let name = scope_name.expect("an import-scoped overload view has a name");
                    if scope.has_explicit(name) {
                        return explicit_packages.contains(&signature.package);
                    }
                    selected_import_level
                        .is_some_and(|level| scope.levels()[level].contains(&signature.package))
                }) || (import_scope.is_none()
                    && packages.is_some_and(|packages| packages.contains(&signature.package)))
                    || (import_scope.is_none() && packages.is_none())
            })
    }

    pub fn first(&self) -> Option<&'symbols Signature> {
        self.iter().next()
    }

    pub fn is_empty(&self) -> bool {
        self.iter().next().is_none()
    }
}

/// Compatibility name for source signatures built in this module. The data itself belongs to the
/// shared callable metadata model and is not constructor-specific.
pub use crate::libraries::DefaultValue as CtorDefaultValue;

/// Capture a primary-constructor parameter default in the file-independent [`CtorDefaultValue`] form, or
/// `None` for an unmodeled default (a non-literal/non-object — kept conservative so behavior matches the
/// previous literal-only handling). Resolves an object-singleton `Name` (`= EmptyCoroutineContext`) to its
/// internal via the type universe, so the default lowers cross-file as `getstatic …INSTANCE`.
pub(super) fn extract_ctor_default(
    file: &File,
    dx: ExprId,
    class_names: &ClassNames,
    libraries: &dyn SymbolSource,
) -> Option<CtorDefaultValue> {
    Some(match file.expr(dx) {
        Expr::IntLit(v) => CtorDefaultValue::Int(*v),
        Expr::LongLit(v) => CtorDefaultValue::Long(*v),
        Expr::DoubleLit(v) => CtorDefaultValue::Double(*v),
        Expr::FloatLit(v) => CtorDefaultValue::Float(*v),
        Expr::BoolLit(v) => CtorDefaultValue::Bool(*v),
        Expr::CharLit(v) => CtorDefaultValue::Char(*v),
        Expr::StringLit(s) => CtorDefaultValue::Str(s.clone()),
        Expr::NullLit => CtorDefaultValue::Null,
        Expr::Name(n) => {
            let internal = class_names.get(n)?;
            if libraries
                .classifier(internal)
                .is_some_and(|t| t.is_object())
            {
                CtorDefaultValue::Object(internal)
            } else {
                return None;
            }
        }
        _ => return None,
    })
}

/// Everything a caller needs about a declared Kotlin class.
#[derive(Clone, Debug)]
pub struct MemberExtFunSig {
    /// Semantic receiver shape, its type parameters symbolic until the call site unifies them: no
    /// source `TypeRef`, descriptor or provider identity, so module and dependency share selection.
    pub(super) receiver_ty: Ty,
    pub(super) physical_receiver: Ty,
    pub(super) signature: Signature,
    /// Exact declaration type-parameter capabilities retained from the normalized provider/header
    /// record. Reconstructing `CallSig` from `Signature` cannot recover this sparse semantic fact.
    pub(super) reified_type_parameter_ordinals: Vec<u32>,
    /// Physical method parameters after the extension receiver, opaque to semantic resolution.
    pub(super) physical_params: Vec<Ty>,
    /// Provider-owned physical method spelling, travelling with the selected target only so a
    /// backend can emit a mangled dependency method; never selection or diagnostic input.
    pub(super) physical_name: String,
    /// Stable provider identity for a dependency declaration. Current-module declarations use
    /// `signature.stable_declaration` instead; exactly one identity is present after selection.
    pub(super) external_identity: Option<crate::fir::ExternalCallableId>,
    pub(super) external_default_provider: Option<crate::fir::ExternalCallableId>,
    pub(super) declared_ret: Option<Ty>,
    pub(super) overridden_results: Box<[Ty]>,
    pub(super) inline_body_plan: Option<Box<crate::libraries::InlineBodyPlan>>,
}

/// Resolved declaration types keyed by the parsed declaration's source identity. Lowering consumes
/// this exact record; it never re-selects or re-pairs an overload by name, arity, or table order.
#[derive(Clone, Debug)]
pub struct SourceMethodPlan {
    pub signature: Signature,
    pub extension_receiver: Option<Ty>,
}

impl MemberExtFunSig {
    pub(crate) fn receiver_ty(&self) -> Ty {
        self.receiver_ty
    }

    pub(crate) fn signature(&self) -> &Signature {
        &self.signature
    }
}
