//! Property declarations and accessor contracts retained by common IR.

use super::*;

/// One inline property use. `accessor` is the declaration the checker selected; `substitutions`
/// already carry each type parameter's semantic name and reified flag.
#[derive(Clone, Debug, PartialEq)]
pub struct IrInlinePropertySplice {
    pub accessor: crate::fir::DeclarationId,
    pub substitutions: Vec<IrInlineTypeSubstitution>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IrInlineTypeSubstitution {
    pub name: String,
    pub reified: bool,
    pub value: Ty,
}

impl IrFile {
    /// A property's source-level accessor parameters and identities, together with its getter.
    ///
    /// Plugins and targets consume this common declaration edge instead of rediscovering a
    /// property from a generated accessor's spelling.
    pub(crate) fn property_accessor_parameters(
        &self,
        class_index: usize,
        source_order: u32,
    ) -> (Vec<Ty>, Vec<IrParameterIdentity>, Option<FunId>) {
        let class = &self.classes[class_index];
        if let Some(property) = class
            .properties
            .iter()
            .find(|property| property.source_order == source_order)
        {
            if let Some(getter) = property.getter {
                let (parameters, identities) = declared_parameters(self, getter);
                return (parameters, identities, Some(getter));
            }
            let mut context_ordinal = 0;
            let identities = property
                .context_params
                .iter()
                .map(|(name, kind, _)| {
                    let identity = match kind {
                        crate::types::ContextParameterKind::Named => {
                            IrParameterIdentity::context_value(name)
                        }
                        crate::types::ContextParameterKind::Anonymous => {
                            IrParameterIdentity::anonymous_context_parameter(context_ordinal)
                        }
                        crate::types::ContextParameterKind::LegacyReceiver => {
                            IrParameterIdentity::context_receiver(context_ordinal)
                        }
                        crate::types::ContextParameterKind::None => {
                            panic!("a property context parameter retains its semantic role")
                        }
                    };
                    context_ordinal += 1;
                    identity
                })
                .collect();
            let parameters = property
                .context_params
                .iter()
                .map(|(_, _, ty)| *ty)
                .collect();
            return (parameters, identities, property.getter);
        }
        if let Some(property) =
            self.member_ext_props
                .get(&class.fq_name_id())
                .and_then(|properties| {
                    properties
                        .iter()
                        .find(|property| property.source_order == source_order)
                })
        {
            let (parameters, identities) = declared_parameters(self, property.getter);
            return (parameters, identities, Some(property.getter));
        }
        if let Some(property) = self
            .companion_blocks
            .properties_of(class.fq_name_id())
            .find(|property| property.source_order == source_order)
        {
            return match property.getter {
                Some(getter) => {
                    let (parameters, identities) = declared_parameters(self, getter);
                    (parameters, identities, Some(getter))
                }
                None => (Vec::new(), Vec::new(), None),
            };
        }
        assert!(
            self.declared_class_statics
                .get(&class.fq_name_id())
                .into_iter()
                .flatten()
                .any(|&static_id| self.statics[static_id as usize].source_order == source_order),
            "a property annotation names a declaration owned by its class"
        );
        (Vec::new(), Vec::new(), None)
    }
}

fn declared_parameters(ir: &IrFile, function: FunId) -> (Vec<Ty>, Vec<IrParameterIdentity>) {
    let parameters = ir.functions[function as usize].params.clone();
    let identities = ir
        .function_parameter_identities(function)
        .map(<[_]>::to_vec)
        .unwrap_or_default();
    assert!(
        parameters.is_empty() || identities.len() == parameters.len(),
        "a property accessor with parameters retains their exact identities"
    );
    (parameters, identities)
}

/// Property-owned index used while checked inline accessors are materialized and spliced.
#[derive(Default)]
pub(crate) struct IrInlinePropertyAccess {
    pub(crate) accessor_functions: crate::fx_hash::HashMap<crate::fir::DeclarationId, FunId>,
    pub(crate) splices: crate::fx_hash::HashMap<ExprId, IrInlinePropertySplice>,
}

/// Where a property's written accessor headers start, where their bodies or bodyless headers end,
/// and where the whole declaration ends, each a 1-based line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IrAccessorLines {
    pub getter: u32,
    pub setter: u32,
    pub getter_end: u32,
    pub setter_end: u32,
    pub declaration_end: u32,
}

/// One source property declaration and the checked bodies that realize its language semantics.
/// Storage and accessor naming remain backend choices; stable property/class identities and final
/// types are already fixed here.
#[derive(Clone, Debug)]
pub struct IrCheckedProperty {
    pub declaration: crate::fir::DeclarationId,
    /// Exact source name span retained for diagnostics that can only be classified after a backend
    /// has selected the declaration's physical representation.
    pub diagnostic_span: Option<crate::diag::Span>,
    /// Exact source accessor declaration spans, beginning at the first accessor annotation when
    /// present. A representation-dependent clash belongs to the accessor, not the whole property.
    pub getter_diagnostic_span: Option<crate::diag::Span>,
    pub setter_diagnostic_span: Option<crate::diag::Span>,
    /// The declaration's stable source position, the key its accessors take in `fn_source_order`.
    pub source_order: u32,
    /// Source declaration line accepted while the bounded Pass-2 syntax unit is live. This is
    /// output metadata, not a source locator: property realization copies it to the common-IR
    /// declarations it creates after the syntax unit has already been dropped.
    pub decl_line: u32,
    /// For a primary-constructor property, the line its declaration starts on with its annotations
    /// included, else 0. Accepted like [`Self::decl_line`] and kept apart from it: the constructor's
    /// store of the property maps here, its accessors to `decl_line`.
    pub decl_start_line: u32,
    /// Lines of written accessors and of the property declaration's end, accepted like
    /// [`Self::decl_line`].
    pub accessor_lines: IrAccessorLines,
    /// Exact semantic position among the owning class's property initializers and `init` blocks.
    /// This is copied from the stable FIR declaration header, never reconstructed from source.
    pub initialization_order: Option<u32>,
    pub class: Option<ClassId>,
    pub name: String,
    pub ty: Ty,
    /// Checked explicit backing-field type, distinct from the public property/accessor type.
    pub storage_ty: Option<Ty>,
    pub visibility: crate::types::Visibility,
    pub flags: crate::fir::DeclarationFlags,
    pub initializer: Option<ExprId>,
    /// Kotlin metadata's `HAS_CONSTANT`: an immutable property of a `const val` type whose checked
    /// initializer is a constant to kotlinc's serializer (see `fir_lower::metadata_constants`).
    pub has_constant_initializer: bool,
    /// A script's `$$result` whose checked initializer is an effect, of type `Unit` or `Nothing`.
    /// kotlinc evaluates it as the script's last statement and keeps no property for it.
    pub script_effect: bool,
    pub delegate: Option<ExprId>,
    pub delegate_plan: Option<crate::fir::FirPropertyDelegatePlan>,
    pub getter: Option<ExprId>,
    pub setter: Option<ExprId>,
    /// Accessor declarations and whether each was declared `inline`. Published with the property
    /// so later expansion does not rediscover the flag from the declaration index.
    pub getter_declaration: Option<crate::fir::DeclarationId>,
    pub setter_declaration: Option<crate::fir::DeclarationId>,
    pub getter_inline: bool,
    pub setter_inline: bool,
}

/// A property a class DECLARES. A property is a declaration, not a pair of methods: `val a: Int` is one
/// thing, and the `getA()` a target may emit for it is a realization of it. The front end lowers only
/// what is genuinely Kotlin — a source-written accessor's BODY — and leaves naming, descriptors and
/// dispatch to the backend.
/// One member EXTENSION property declaration ([`IrFile::member_ext_props`]): the SEMANTIC types
/// (checker-resolved, pre-erasure) plus the accessor realization, everything a class `Property`
/// metadata record needs.
#[derive(Clone, Debug)]
pub struct MemberExtProp {
    pub name: String,
    /// Exact source spans for diagnostics about the declaration and its accessors. Dependency and
    /// compiler-generated member extensions have no source location.
    pub diagnostic_span: Option<crate::diag::Span>,
    pub getter_diagnostic_span: Option<crate::diag::Span>,
    pub setter_diagnostic_span: Option<crate::diag::Span>,
    /// Stable source declaration order. This identifies the property for declaration-owned facts
    /// such as its checked type spelling; `name` is metadata payload and may be shared by overloads.
    pub source_order: u32,
    /// Declared extension receiver (`Int` in `val Int.doubled`).
    pub receiver: crate::types::Ty,
    /// Declared property type.
    pub ty: crate::types::Ty,
    pub is_var: bool,
    /// Whether this declaration has no accessor implementation and must be realized abstractly.
    pub is_abstract: bool,
    pub modifiers: IrPropertyModifiers,
    /// See [`IrProperty::delegate_field`].
    pub delegate_field: Option<u32>,
    /// Getter function id. Abstract properties point at a bodyless common-IR function.
    pub getter: u32,
    /// Setter function id, for a `var`.
    pub setter: Option<u32>,
    pub visibility: crate::types::Visibility,
    /// Declaration-owned generic parameters. Source names are metadata payload; semantic names are
    /// the stable identities used by `receiver` and `ty`.
    pub type_params: Vec<IrTypeParameter>,
    /// Annotations written on this property's accessor declarations.
    pub accessor_annotations: super::AccessorAnnotations,
}

/// How a member property is declared, as Kotlin metadata records it; no JVM shape encodes these.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IrPropertyModifiers {
    pub modality: IrPropertyModality,
    /// Source declares the getter, or the property is delegated; an accessor the compiler
    /// supplies on its own is the default one.
    pub declared_getter: bool,
    /// Source wrote the setter's body, or the property is a delegated `var`. A bodiless `set`
    /// keeps the default implementation; whether it narrows visibility is recorded separately.
    pub declared_setter: bool,
    pub delegated: bool,
    pub lateinit: bool,
    pub member_kind: IrMemberKind,
}

/// How a class member came to be, as Kotlin metadata's `MEMBER_KIND` records it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum IrMemberKind {
    #[default]
    Declaration,
    /// A forwarder to an interface delegate (`class C(d: I) : I by d`).
    Delegation,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum IrPropertyModality {
    #[default]
    Final,
    /// `open`, an `override` not marked `final`, or an interface member with a getter.
    Open,
    /// `abstract`, or an interface member without a getter.
    Abstract,
}

#[derive(Clone, Debug)]
pub struct IrProperty {
    pub name: String,
    /// Exact source property span for a representation-dependent declaration diagnostic. Generated
    /// properties have no source declaration and leave this absent.
    pub diagnostic_span: Option<crate::diag::Span>,
    pub getter_diagnostic_span: Option<crate::diag::Span>,
    pub setter_diagnostic_span: Option<crate::diag::Span>,
    /// Named context parameters in source order. Metadata records these separately from ordinary
    /// value parameters, and checked call sites supply their operands implicitly.
    pub context_params: Vec<(String, crate::types::ContextParameterKind, Ty)>,
    /// Stable source declaration order and 1-based declaration line. These remain attached to the
    /// declaration so a backend can order/debug synthesized accessors without rebinding the
    /// property by spelling.
    pub source_order: u32,
    pub decl_line: u32,
    /// Source lines retained for backend-synthesized accessors after their functions have been
    /// materialized. In particular, a bodiless private setter can delimit a `lateinit` getter.
    pub accessor_lines: IrAccessorLines,
    /// The property's language-level type. This is also the type exposed by its accessors.
    pub ty: Ty,
    /// Type parameters this property declares (`var <X, Y> ctx: Map<X, Y>`). They are not the
    /// enclosing class's parameters. Metadata and each accessor's generic signature publish them.
    pub type_params: Vec<IrTypeParameter>,
    /// Kotlin declaration visibility. Backends consume this semantic fact when choosing the
    /// visibility of an accessor or a target-specific storage realization; they must not recover it
    /// from a rendered owner/property-name key.
    pub visibility: crate::types::Visibility,
    /// Checked Kotlin return-value status, inherited from the property this one overrides.
    pub return_value_status: crate::types::ReturnValueStatus,
    /// Resolved Kotlin annotation identities. Backends interpret annotations in their own namespace;
    /// common lowering does not turn them into storage or calling-convention choices.
    pub annotations: Box<[TypeName]>,
    /// The declaration initializer after common lowering, before any backend chooses storage.
    /// `None` means the declaration has no initializer (or its source shape is not represented),
    /// which is distinct from an explicit nullable initializer lowered to `IrConst::Null`.
    /// Keeping this on the declaration lets a backend relocate storage without re-reading the AST
    /// or mistaking a later assignment in an `init` block for the declaration initializer.
    pub initializer: Option<ExprId>,
    /// The declared type of an explicit backing field when it differs from the property's public type.
    /// The JVM value-class pass may erase the physical [`IrField`] to its carrier, so retaining this
    /// semantic storage boundary lets the backend box/unbox at the accessor without resolving anything.
    pub storage_ty: Option<Ty>,
    /// Index into [`IrClass::fields`] for the backing field, `None` for a computed/delegated property
    /// (which stores nothing).
    pub backing_field: Option<u32>,
    pub is_var: bool,
    /// Non-final: the accessor a backend emits for it must be overridable.
    pub is_open: bool,
    pub modifiers: IrPropertyModifiers,
    /// Index into [`IrClass::fields`] of a delegated property's `x$delegate` field, which Kotlin
    /// metadata names as the property's JVM field.
    pub delegate_field: Option<u32>,
    /// A `private` property. kotlinc emits NO accessor for one — in-class reads go straight to the
    /// backing field — so a use from outside the declaring class has nothing to call, and whichever
    /// path is lowering it does not own the access.
    /// See [`IrCheckedProperty::has_constant_initializer`].
    pub has_constant_initializer: bool,
    pub is_private: bool,
    /// The resolved visibility of a `var`'s setter: its own modifier (`private set`, `protected set`),
    /// else the property's. This is declaration visibility, not a JVM flag: every backend must
    /// preserve it when realizing a default setter or the storage it guards.
    pub setter_visibility: crate::types::Visibility,
    /// The lowered body of a source-written getter/setter (a computed, `field`-using, or delegated
    /// property). `None` for a plain backing-field property, whose accessor has no source body at all.
    pub getter: Option<FunId>,
    pub setter: Option<FunId>,
    /// The JVM name a backend must use for the synthesized accessor when the plain spelling is wrong —
    /// a value-class-typed property's accessor is `@JvmName`-mangled. Stamped by the pass that knows the
    /// value classes; `None` means the ordinary spelling applies.
    pub getter_jvm_name: Option<String>,
    pub setter_jvm_name: Option<String>,
    /// Some use of a private property side reaches it from outside the declaring class — either the
    /// whole property is private, or a write reaches a private default setter. A target records the
    /// representation bridge that preserves that checked access; silently changing the call or
    /// storage path instead would change what the program does.
    pub needs_access_bridge: bool,
    /// Annotations written on this property's accessors, whether source declared each accessor or
    /// a backend realizes its default.
    pub accessor_annotations: super::AccessorAnnotations,
}

impl IrProperty {
    /// Whether this mutable declaration has a compiler-default setter whose resolved visibility is
    /// private. This is a stable Kotlin declaration fact: it does not depend on whether a backend has
    /// materialized an accessor function.
    pub(crate) fn has_private_default_setter(&self) -> bool {
        self.is_var && !self.modifiers.declared_setter && self.setter_visibility.is_private()
    }
}

/// A property declared by another file of the module, as the file's code selects it.
#[derive(Clone, Debug, PartialEq)]
pub struct IrModuleProperty {
    pub source: IrModuleSource,
    pub name: String,
    pub ty: Ty,
    pub context_parameters: Vec<Ty>,
    pub extension_receiver: Option<Ty>,
    pub mutable: bool,
    pub owner: Option<TypeName>,
    /// Source-level kind of `owner`. Common IR retains the Kotlin declaration fact; a target backend
    /// decides whether that kind uses interface dispatch, singleton storage, or another physical form.
    pub owner_kind: Option<IrClassifierKind>,
    pub companion_associated: bool,
    /// Outer classifier whose companion object owns this declaration. This is the Kotlin
    /// singleton-association edge; it says nothing about target storage.
    pub companion_owner: Option<TypeName>,
    pub visibility: crate::types::Visibility,
    pub setter_visibility: crate::types::Visibility,
    /// The setter's value parameter, as the declaration names it; `None` for a `val`.
    pub setter_parameter: Option<crate::fir::ResolvedParameterIdentity>,
    /// Resolved Kotlin annotation identities. A target backend may interpret annotations in its
    /// namespace; common lowering never turns one into a physical access kind.
    pub annotations: Box<[TypeName]>,
    /// The annotations that land on the getter, whether written on it or targeted at it with
    /// `@get:`, with their constant string arguments. A target backend interprets the ones in its
    /// namespace for the accessor it emits.
    pub getter_annotations: Box<[super::IrHeaderAnnotation]>,
    /// The annotations that land on the setter (written on it or targeted with `@set:`).
    pub setter_annotations: Box<[super::IrHeaderAnnotation]>,
    pub flags: crate::fir::DeclarationFlags,
    /// Provider-normalized payload of a `const val`. Uses consume the value without reopening the
    /// declaration or selecting target storage by owner/spelling.
    pub compile_time_constant: Option<IrConst>,
    /// Where the property lives when `owner` is absent.
    pub placement: IrStaticPlacement,
    /// The own, unapplied types of the properties this one overrides, as the override edges its
    /// classifier published them (see [`super::IrModuleCallable::overridden_results`]).
    pub overridden_types: Box<[Ty]>,
}
