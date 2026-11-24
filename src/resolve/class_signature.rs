use super::*;

/// Bit-packed boolean modifiers for a [`ClassSig`]. The per-class flags below each cost a full
/// byte as a separate `bool` field (plus struct padding); collapsed into one `u16` they save several
/// bytes per sig, and the compiler builds a few thousand. Built with the `with_*` chain from
/// [`ClassFlags::default`]; read through the `ClassSig::is_*` / `has_*` accessors.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClassFlags(u16);

impl ClassFlags {
    pub(super) const INTERFACE: u16 = 1 << 0;
    pub(super) const OBJECT: u16 = 1 << 1;
    pub(super) const ABSTRACT: u16 = 1 << 2;
    pub(super) const FUN_INTERFACE: u16 = 1 << 3;
    pub(super) const SEALED: u16 = 1 << 4;
    pub(super) const FINAL: u16 = 1 << 5;
    const HAS_ABSTRACT_MEMBERS: u16 = 1 << 6;
    pub(super) const ANNOTATION: u16 = 1 << 7;
    const DATA: u16 = 1 << 8;
    const ENUM: u16 = 1 << 9;

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
    pub(super) const fn has(self, mask: u16) -> bool {
        self.0 & mask != 0
    }

    #[inline]
    pub const fn with_interface(self, on: bool) -> Self {
        self.with(Self::INTERFACE, on)
    }
    #[inline]
    pub const fn with_object(self, on: bool) -> Self {
        self.with(Self::OBJECT, on)
    }
    #[inline]
    pub const fn with_abstract(self, on: bool) -> Self {
        self.with(Self::ABSTRACT, on)
    }
    #[inline]
    pub const fn with_fun_interface(self, on: bool) -> Self {
        self.with(Self::FUN_INTERFACE, on)
    }
    #[inline]
    pub const fn with_sealed(self, on: bool) -> Self {
        self.with(Self::SEALED, on)
    }
    #[inline]
    pub const fn with_final(self, on: bool) -> Self {
        self.with(Self::FINAL, on)
    }
    #[inline]
    pub const fn with_has_abstract_members(self, on: bool) -> Self {
        self.with(Self::HAS_ABSTRACT_MEMBERS, on)
    }
    #[inline]
    pub const fn with_annotation(self, on: bool) -> Self {
        self.with(Self::ANNOTATION, on)
    }
    #[inline]
    pub const fn with_data(self, on: bool) -> Self {
        self.with(Self::DATA, on)
    }
    #[inline]
    pub const fn with_enum(self, on: bool) -> Self {
        self.with(Self::ENUM, on)
    }
}

/// The compiler-built array operation a call checks as, and the label of its initializer lambda
/// in diagnostics when it is a size constructor.
#[derive(Clone, Copy)]
pub(super) struct ArrayBuiltin {
    pub(super) kind: crate::synthetics::SyntheticKind,
    pub(super) initializer_label: Option<&'static str>,
}

impl ArrayBuiltin {
    pub(super) fn of(kind: crate::synthetics::SyntheticKind) -> Self {
        Self {
            kind,
            initializer_label: None,
        }
    }

    pub(super) fn constructor(synthetic: &crate::synthetics::Synthetic) -> Self {
        Self {
            kind: synthetic.kind,
            initializer_label: Some(synthetic.name),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct SourceClassHeader {
    pub(super) flags: ClassFlags,
    pub(super) direct_supertypes: crate::types::TypeNameList,
}

/// One source of truth for declaration-level classifier flags. The same packed value seeds the
/// all-files header index and is later installed on the complete `ClassSig`, preventing early
/// inference and final checking from classifying a declaration differently.
pub(super) fn source_class_flags(class: &ClassDecl) -> ClassFlags {
    ClassFlags::default()
        .with_interface(class.is_interface())
        .with_object(class.is_singleton())
        .with_abstract(class.is_abstract())
        .with_fun_interface(class.is_fun_interface)
        .with_sealed(class.is_sealed())
        .with_final(class.is_final())
        .with_has_abstract_members(
            class.methods.iter().any(|method| method.is_abstract())
                || class.body_props.iter().any(|property| property.is_abstract),
        )
        .with_annotation(class.is_annotation())
        .with_data(class.is_data)
        .with_enum(class.is_enum())
}

/// Classifier flags read from the compact Pass-1 declaration inventory.
///
/// `HAS_ABSTRACT_MEMBERS` is a property of the classifier's direct declaration children, not of
/// its parser container. Computing it from stable ownership lets production bootstrap source
/// classifiers after the corresponding `ClassDecl` has been destroyed.
pub(super) fn streamed_source_class_flags(
    headers: &crate::fir::StreamedHeaderModule,
    classifier: &crate::fir::DeclarationStub,
) -> ClassFlags {
    let flags = classifier.flags;
    let has_abstract_members = headers.stubs.iter().any(|candidate| {
        candidate.flags.has(crate::fir::DeclarationFlags::ABSTRACT)
            && matches!(
                candidate.kind,
                crate::fir::DeclarationKind::Function
                    | crate::fir::DeclarationKind::Property
                    | crate::fir::DeclarationKind::Accessor
            )
            && headers
                .declarations
                .anchor(candidate.id)
                .is_some_and(|anchor| anchor.owner == Some(classifier.id))
    });
    ClassFlags::default()
        .with_interface(flags.has(crate::fir::DeclarationFlags::INTERFACE))
        .with_object(flags.has(crate::fir::DeclarationFlags::SINGLETON))
        .with_abstract(flags.has(crate::fir::DeclarationFlags::ABSTRACT))
        .with_fun_interface(flags.has(crate::fir::DeclarationFlags::FUN_INTERFACE))
        .with_sealed(flags.has(crate::fir::DeclarationFlags::SEALED))
        .with_final(flags.has(crate::fir::DeclarationFlags::FINAL))
        .with_has_abstract_members(has_abstract_members)
        .with_annotation(flags.has(crate::fir::DeclarationFlags::ANNOTATION_CLASS))
        .with_data(flags.has(crate::fir::DeclarationFlags::DATA))
        .with_enum(flags.has(crate::fir::DeclarationFlags::ENUM))
}

/// Capture the source-level callable inventory before functions and properties are normalized into
/// separate name-indexed maps. Byte offsets are parser facts, so this preserves interleaving without
/// deriving order later from arena indices or stable identities.
pub(super) fn source_declared_callable_order(class: &ClassDecl) -> Vec<String> {
    let mut declarations = Vec::new();
    declarations.extend(
        class
            .props
            .iter()
            .filter(|property| property.is_property)
            .map(|property| (property.span.lo, property.name.clone())),
    );
    declarations.extend(
        class
            .methods
            .iter()
            .map(|method| (method.span.lo, method.name.clone())),
    );
    declarations.extend(
        class
            .body_props
            .iter()
            .map(|property| (property.span.lo, property.name.clone())),
    );
    declarations.sort_by_key(|(position, _)| *position);
    let mut names = Vec::new();
    for (_, name) in declarations {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// Direct callable names of a compact classifier in lexical declaration order.
///
/// This is declaration metadata, not executable-body location. The compact inventory already owns
/// every direct member's stable owner and source order, so production need not retain the enclosing
/// `ClassDecl` merely to reconstruct function/property interleaving.
pub(super) fn streamed_declared_callable_order(
    headers: &crate::fir::StreamedHeaderModule,
    classifier: crate::fir::DeclarationId,
) -> Vec<String> {
    let declarations = headers.owned_stubs(classifier).filter_map(|stub| {
        matches!(
            stub.kind,
            crate::fir::DeclarationKind::Function | crate::fir::DeclarationKind::Property
        )
        .then(|| {
            headers
                .lookup_names
                .get(stub.lookup_name?)
                .map(str::to_owned)
        })
        .flatten()
    });
    let mut names = Vec::new();
    for name in declarations {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

pub(super) fn streamed_declaration_is_within(
    headers: &crate::fir::StreamedHeaderModule,
    declaration: crate::fir::DeclarationId,
    classifier: crate::fir::DeclarationId,
) -> bool {
    let mut current = Some(declaration);
    while let Some(candidate) = current {
        if candidate == classifier {
            return true;
        }
        current = headers
            .declarations
            .stable_anchor(candidate)
            .and_then(|anchor| anchor.owner);
    }
    false
}

pub(super) fn compact_generated_member_declaration(
    headers: Option<&crate::fir::StreamedHeaderModule>,
    owner: Option<crate::fir::DeclarationId>,
    name: &str,
) -> Option<crate::fir::DeclarationId> {
    let headers = headers?;
    let owner = owner?;
    headers.owned_stubs(owner).find_map(|stub| {
        (stub.kind == crate::fir::DeclarationKind::Function
            && stub
                .flags
                .has(crate::fir::DeclarationFlags::COMPILER_GENERATED)
            && stub
                .lookup_name
                .and_then(|lookup| headers.lookup_names.get(lookup))
                == Some(name))
        .then_some(stub.id)
    })
}

#[derive(Clone, Debug)]
pub struct DeclaredPropertySig {
    pub ty: Ty,
    pub storage_ty: Option<Ty>,
    pub visibility: Visibility,
    /// Whether this declaration participates in Kotlin source member lookup. A captured lexical
    /// value may need typed classifier storage while its member bodies are checked, but that
    /// storage is not a Kotlin property and cannot shadow a real inherited declaration.
    pub source_visible: bool,
    /// A source `const val` is realized as a static field and deliberately has no instance getter.
    /// Keep that ABI fact in the semantic declaration handoff: consumers that only see a module
    /// signature must not mistake the conventional `getter_name` spelling for an emitted accessor.
    pub is_const: bool,
    /// Resolved declaration annotation identities. This is Kotlin semantic metadata; a target may
    /// interpret its own annotations when choosing a physical representation.
    pub annotations: Vec<TypeName>,
    pub getter_name: String,
    pub setter_name: Option<String>,
    pub setter_parameter_name: Option<String>,
    /// Visibility of the setter declaration. `None` means `val`; a `var` normally inherits the
    /// property's visibility, while an explicit `private set` narrows only this value. Keeping it beside
    /// `setter_name` prevents backend accessor synthesis from accidentally widening the source ABI.
    pub setter_visibility: Option<Visibility>,
    /// `true` for a property with a custom getter or a delegate — a re-read can then observe a
    /// different value than an earlier proof, so flow narrowing (smart casts) must skip it
    /// (kotlinc: "smart cast is impossible, because the property has a custom getter").
    pub has_custom_getter: bool,
    /// The property declares no accessor implementation. Providers carry this semantic modality
    /// beside the accessor handles so consumers never infer a body from source/classpath origin.
    pub is_abstract: bool,
    /// Whether this property's accessor remains overridable. Smart-cast stability depends on the
    /// selected property's modality, not merely on whether its declaring class is open: a default
    /// `val` is final even in an open class, while an `open val` becomes stable when viewed through
    /// a statically final receiver type because no runtime subclass can replace its getter.
    pub is_open: bool,
    pub context_params: Vec<Ty>,
    pub context_param_names: Vec<String>,
    pub context_parameter_identities: Vec<crate::fir::ResolvedParameterIdentity>,
    pub source_member: Option<crate::libraries::SourceMember>,
    pub stable_declaration: Option<crate::fir::DeclarationId>,
}

#[derive(Clone, Debug)]
pub struct ClassSig {
    pub declaration_name: Option<crate::types::ClassifierDeclarationName>,
    pub internal: TypeName,
    /// Stable frontend identity for a source classifier. Parser `DeclId` remains a temporary
    /// collection coordinate and is never needed by signature solving or checked FIR.
    pub stable_declaration: Option<crate::fir::DeclarationId>,
    /// File that declares this source classifier. Class visibility cannot be decided from the module
    /// alone: a top-level `private` class is visible only to declarations in this same file.
    pub source_file: u32,
    /// Arena declaration that owns this source classifier. Synthetic companion classifiers have no
    /// independent class declaration and therefore leave this unset.
    pub source_decl: Option<DeclId>,
    /// The classifier's NAME in [`Self::source_file`], where a plugin's class rule reports. A
    /// synthetic companion has no name of its own and leaves this empty.
    pub name_span: crate::diag::Span,
    /// Whether the declaration is structurally owned by another classifier. This is captured from
    /// the declaration graph while source ownership is live; an internal-name `$` is not ownership.
    pub is_nested: bool,
    pub visibility: Visibility,
    /// Resolved classifier declaration annotations, projected to the stable module index before
    /// Pass 2 so plugins and semantic checks never revisit source occurrences.
    pub annotations: Vec<TypeName>,
    /// Fully checked applications of those annotations. This is populated while the Pass-1
    /// declaration expression arena is live, then projected by stable declaration identity.
    pub applied_annotations: Vec<crate::types::ResolvedAnnotation>,
    /// Resolved CLASS arguments of those annotations, by annotation ordinal — the identity behind
    /// `@Serializable(with = X::class)`. Resolved here, with the annotation's own name and through
    /// the same classifier rules, because no later phase may recover it from a spelling.
    pub annotation_class_arguments: Vec<(u32, TypeName)>,
    /// Exact identities of plugin-generated nested classifiers. The generating frontend extension
    /// publishes these with the source header; backends never infer them from annotations or names.
    pub generated_nested_classifiers: Vec<crate::types::GeneratedClassifierFact>,
    pub props: Vec<(String, Ty, bool)>, // backing-field properties (name, type, is_var)
    pub declared_props: HashMap<String, DeclaredPropertySig>,
    /// Context-parameter properties are overloadable by their context shape and may share a name
    /// with an ordinary stored property.
    pub contextual_props: HashMap<String, Vec<DeclaredPropertySig>>,
    /// Compile-time values of source constants physically owned by this classifier. A checker read
    /// copies the selected value onto the expression; lowering never searches by source spelling.
    pub constants: HashMap<String, crate::libraries::LibraryConst>,
    /// Extension properties declared inside this class/object.
    pub member_ext_props: HashMap<String, Vec<MemberExtPropSig>>,
    /// Member extension functions keyed by source name.
    pub member_ext_funs: HashMap<String, Vec<MemberExtFunSig>>,
    pub has_primary_ctor: bool,
    pub primary_constructor_declaration: Option<crate::fir::DeclarationId>,
    /// Resolved annotations on the primary constructor declaration. These are signature/header
    /// facts and are projected into the stable module index before Pass 2.
    pub primary_constructor_annotations: Vec<TypeName>,
    /// Full semantic primary-constructor parameter types in order (includes non-property params;
    /// a vararg slot is its array type, while the AST retains the written element type).
    pub ctor_params: Vec<Ty>,
    /// Symbolic constructor parameter types and their definitely-non-null marker.
    pub ctor_param_shapes: Vec<(Ty, bool)>,
    /// Primary-constructor parameter NAMES, in order, and whether each declares a default. Needed to
    /// map a named-argument call (`C(b = 9)`) onto positions from ANY file in the module — the AST
    /// declaration is only reachable from the file that declares it.
    pub ctor_param_names: Vec<(String, bool)>,
    /// Per primary-constructor parameter, the resolved declaration carries Kotlin's internal
    /// `@ImplicitIntegerCoercion`. This is stable declaration behavior, not body syntax.
    pub ctor_implicit_integer_coercion: Vec<bool>,
    pub ctor_vararg: Option<usize>,
    pub methods: MethodMap,
    /// Direct callable names in exact lexical declaration order. This is captured while the class
    /// AST is present because the semantic maps deliberately group functions and properties by
    /// name and cannot reconstruct their interleaving afterwards.
    pub declared_callable_order: Vec<String>,
    pub source_methods: Vec<SourceMethodPlan>,
    /// Bit-packed class modifiers (`interface`, `object`, `abstract`, `fun interface`, `sealed`,
    /// `final`, `has_abstract_members`, `annotation`). Read via the `is_*`/`has_*` accessors; the
    /// `annotation` bit lives here too (its doc stays at the former field position below).
    pub flags: ClassFlags,
    /// Declared enum entry names in source order. Non-empty only for an `enum class` (the
    /// [`ClassFlags::ENUM`] kind); an enum is an ordinary classifier of that kind, not a separate
    /// table.
    pub enum_entries: Vec<String>,
    /// `Some(outer_internal)` for an `inner class` — it captures the enclosing instance (a `this$0`
    /// field of the outer type); constructed as `outerInstance.Inner(...)`.
    pub inner_of: Option<TypeName>,
    /// The declared companion classifier, independent of whether it owns any methods or properties.
    pub companion_internal: Option<TypeName>,
    /// Names of `lateinit` properties (instance and companion) — reads emit a null-check that throws.
    pub lateinit_props: crate::fx_hash::HashSet<String>,
    /// Internal names of interfaces this type implements (for subtyping).
    pub interfaces: crate::types::TypeNameList,
    /// Declaration-scoped type arguments, parallel to [`Self::interfaces`].
    pub interface_type_args: Vec<Vec<Ty>>,
    /// Applied interfaces whose implementations are forwarded through a constructor value by
    /// Kotlin's `Interface by delegate` convention.
    pub delegated_interfaces: Vec<Ty>,
    /// Exact function-type supertype, preserving receiver/context/suspend shape that a classifier
    /// name such as `Function1` cannot encode on its own.
    pub callable_signature: Option<Ty>,
    /// All directly declared function-type supertypes. `callable_signature` remains the unique/first
    /// compatibility view for consumers that cannot accept an intersection.
    pub callable_signatures: Vec<Ty>,
    /// Internal name of the base class (`: Base(..)`), if any.
    pub super_internal: Option<TypeName>,
    /// How many of [`Self::interfaces`] the declaration lists before [`Self::super_internal`].
    pub interfaces_before_superclass: u32,
    /// Applied type arguments on [`Self::super_internal`].
    pub super_type_args: Vec<Ty>,
    /// The parameter types of the base constructor that this class's `super(args)` targets, as the
    /// CHECKER resolved it (uniformly for a same-file, module, or classpath base, via the symbol
    /// source). Empty when the class has no base arguments. The lowerer emits `super(args)` against
    /// these instead of re-resolving the constructor itself.
    pub super_ctor_params: Vec<Ty>,
    /// Each primary-constructor parameter's default, captured FILE-INDEPENDENTLY (see
    /// [`CtorDefaultValue`]) so a different file can fill it without dereferencing a cross-file `ExprId`.
    /// `None` = required (or an unmodeled default → treated as required → skip).
    pub ctor_defaults: Vec<Option<CtorDefaultValue>>,
    /// Semantic secondary-constructor parameter-type lists (`constructor(p…) : this(…)`); a vararg
    /// slot is its array type. A construction call resolves to one of these when its arguments don't
    /// match the primary.
    pub secondary_ctors: Vec<Vec<Ty>>,
    /// The same declarations with classifier type parameters preserved. Stable FIR signatures use
    /// these source shapes; `secondary_ctors` remains the erased legacy selection shape.
    pub secondary_ctor_shapes: Vec<Vec<Ty>>,
    /// Source-level argument mapping facts for each secondary constructor. These are declaration
    /// header facts, not retained default-expression bodies: names, omitted-default positions,
    /// function-parameter shapes, and the vararg slot travel with the normalized callable candidate.
    pub secondary_ctor_call_sigs: Vec<CallSig>,
    pub secondary_constructor_declarations: Vec<Option<crate::fir::DeclarationId>>,
    /// Resolved annotations parallel to [`Self::secondary_constructor_declarations`].
    pub secondary_constructor_annotations: Vec<Vec<TypeName>>,
    /// This class's own type parameters, in declaration order (`class Box<T, U>` → `["T", "U"]`).
    /// Lets a member read substitute the receiver's type arguments for a property whose declared
    /// type is one of these parameters.
    pub type_parameters: crate::types::TypeParameters<Vec<Ty>>,
    /// Upper bounds beyond the first, parallel to `type_parameters`. `Ty::TyParam` intentionally
    /// carries only the first/JVM-erasure bound; source and dependency symbol providers retain the
    /// remaining intersection members here so contextual uses can perform ordinary member lookup.
    pub type_parameter_extra_bounds: Vec<Vec<Ty>>,
    /// Type parameters captured through an `inner`/local classifier's enclosing instance. These are
    /// not declared by this classifier and therefore do not participate in its explicit type-argument
    /// arity, but member signatures may mention them and must substitute them from the bound outer.
    pub captured_type_parameters: crate::types::TypeParameters<Vec<Ty>>,
    /// The same captured declaration identities in Kotlin metadata ID order: outermost declaration
    /// first. Applied receiver arguments deliberately use the opposite, nearest-owner-first order.
    pub metadata_captured_type_parameters: Vec<String>,
    /// Explicit semantic bounds parallel to `tparam_names`; `Error` means no explicit bound.
    /// Properties whose *declared* type is exactly one of this class's type parameters, mapped to
    /// that parameter's index (`class Box<T>(val x: T)` → `{"x": 0}`). A read of such a property on
    /// `Box<Int>` substitutes the argument at that index (`Int`) for the erased `Object`.
    pub generic_props: HashMap<String, (usize, bool)>,
    /// Properties whose DECLARED type is a direct nullable type parameter (`val value: T?`), by
    /// class-type-parameter index. This is separate from `generic_props`, which represents a bare
    /// `T`, and from the recursive shapes in `generic_property_shapes`; all three are semantic
    /// declaration templates consumed uniformly by property selection.
    pub nullable_tparam_props: HashMap<String, usize>,
    /// Function-property signatures that require class type-parameter substitution.
    /// Declared property types that mention class parameters anywhere in their shape (`Cell<T>`,
    /// `(T) -> R`, and similar). Read-site substitution consumes this uniformly; callable-specific
    /// behavior is decided only after the resulting type is known.
    pub generic_property_shapes: HashMap<String, Ty>,
    /// For a `@JvmInline value class X(val v: U)` — the sole underlying property's `(name, type U)`.
    /// A value-class value is represented unboxed as `U`; `X` carries static `box-impl`/`unbox-impl`/
    /// `constructor-impl` members for boxed contexts. `None` for an ordinary class and for a boxed
    /// `value class` enabled by `FullValueClasses`.
    pub value_field: Option<(String, Ty)>,
    /// Boxed `value class` (`+FullValueClasses`, no `@JvmInline`): structural `equals`/`hashCode`/
    /// `toString`, no `componentN`/`copy`, and no unboxed carrier.
    pub full_value: bool,
    /// For a generic higher-order method (`fun <R> map(f: (T) -> R): R`), the un-erased declared shape
    /// needed to substitute the receiver's type arguments into the lambda parameter types and infer the
    /// method's own type parameters from the lambda body. Keyed by method name; only methods whose
    /// substitution actually depends on a type parameter are recorded (ordinary methods are absent).
    pub generic_methods: HashMap<String, Vec<GenericMethod>>,
}

impl std::ops::Deref for ClassSig {
    type Target = crate::types::TypeParameters<Vec<Ty>>;

    fn deref(&self) -> &Self::Target {
        &self.type_parameters
    }
}

pub(super) struct DeclaredCallableClassHeader {
    pub(super) declaration_name: Option<crate::types::ClassifierDeclarationName>,
    pub(super) internal: TypeName,
    pub(super) stable_declaration: Option<crate::fir::DeclarationId>,
    pub(super) source_file: u32,
    pub(super) source_decl: DeclId,
    pub(super) is_nested: bool,
    pub(super) visibility: Visibility,
    pub(super) flags: ClassFlags,
    pub(super) methods: MethodMap,
    pub(super) declared_callable_order: Vec<String>,
    pub(super) member_ext_funs: HashMap<String, Vec<MemberExtFunSig>>,
    pub(super) companion_internal: Option<TypeName>,
    pub(super) direct_supertypes: crate::types::TypeNameList,
    pub(super) direct_supertype_arguments: Vec<Vec<Ty>>,
    pub(super) type_parameters: crate::types::TypeParameters<Vec<Ty>>,
    pub(super) type_parameter_extra_bounds: Vec<Vec<Ty>>,
    pub(super) captured_type_parameters: crate::types::TypeParameters<Vec<Ty>>,
}

impl ClassSig {
    /// A source classifier header used while its properties and inferred-return members are still
    /// being collected. Declared callable signatures are already complete semantic candidates, so
    /// publishing them here lets initializer inference use the ordinary module symbol source and
    /// overload selector. The authoritative class pass replaces this header with the complete
    /// signature before checking or lowering.
    pub(super) fn with_declared_callable_headers(header: DeclaredCallableClassHeader) -> Self {
        let DeclaredCallableClassHeader {
            declaration_name,
            internal,
            stable_declaration,
            source_file,
            source_decl,
            is_nested,
            visibility,
            flags,
            methods,
            declared_callable_order,
            member_ext_funs,
            companion_internal,
            direct_supertypes,
            direct_supertype_arguments,
            type_parameters,
            type_parameter_extra_bounds,
            captured_type_parameters,
        } = header;
        Self {
            declaration_name,
            internal,
            stable_declaration,
            source_file,
            source_decl: Some(source_decl),
            name_span: crate::diag::Span::new(0, 0),
            is_nested,
            visibility,
            annotations: Vec::new(),
            applied_annotations: Vec::new(),
            annotation_class_arguments: Vec::new(),
            generated_nested_classifiers: Vec::new(),
            props: Vec::new(),
            declared_props: HashMap::default(),
            contextual_props: HashMap::default(),
            constants: HashMap::default(),
            member_ext_props: HashMap::default(),
            member_ext_funs,
            has_primary_ctor: false,
            primary_constructor_declaration: None,
            primary_constructor_annotations: Vec::new(),
            ctor_params: Vec::new(),
            ctor_param_shapes: Vec::new(),
            ctor_param_names: Vec::new(),
            ctor_implicit_integer_coercion: Vec::new(),
            ctor_vararg: None,
            methods,
            declared_callable_order,
            source_methods: Vec::new(),
            flags,
            enum_entries: Vec::new(),
            inner_of: None,
            companion_internal,
            lateinit_props: Default::default(),
            // Header inference only needs the semantic hierarchy, not the class/interface JVM
            // partition. Publishing every direct parent here keeps inherited candidate traversal
            // on the common provider path; the complete signature restores the representation split.
            interfaces: direct_supertypes,
            interface_type_args: direct_supertype_arguments,
            delegated_interfaces: Vec::new(),
            callable_signature: None,
            callable_signatures: Vec::new(),
            super_internal: None,
            interfaces_before_superclass: 0,
            super_type_args: Vec::new(),
            super_ctor_params: Vec::new(),
            ctor_defaults: Vec::new(),
            secondary_ctors: Vec::new(),
            secondary_ctor_shapes: Vec::new(),
            secondary_ctor_call_sigs: Vec::new(),
            secondary_constructor_declarations: Vec::new(),
            secondary_constructor_annotations: Vec::new(),
            type_parameters,
            type_parameter_extra_bounds,
            captured_type_parameters,
            metadata_captured_type_parameters: Vec::new(),
            generic_props: HashMap::default(),
            nullable_tparam_props: HashMap::default(),
            generic_property_shapes: HashMap::default(),
            value_field: None,
            full_value: false,
            generic_methods: HashMap::default(),
        }
    }

    pub fn type_params(&self) -> &Vec<String> {
        &self.type_parameters.type_params
    }

    pub fn type_param_bounds(&self) -> &Vec<Ty> {
        &self.type_parameters.type_param_bounds
    }

    pub fn type_param_variances(&self) -> &Vec<crate::types::TypeVariance> {
        &self.type_parameters.type_param_variances
    }
}

/// The symbolic declaration shape of a generic member method.
#[derive(Clone, Debug)]
pub struct GenericMethod {
    /// The declaration's one normalized generic contract. Keeping the full [`GenericSig`] avoids
    /// flattening an absent or intersection bound into a second member-only representation.
    pub signature: GenericSig,
    /// Erased checker parameter types used to identify the selected overload.
    pub params: Vec<Ty>,
    pub param_names: Vec<String>,
}

pub(super) struct GenericMemberPlan {
    pub(super) method: GenericMethod,
    pub(super) class_bindings: crate::types::TyParamBindings,
    pub(super) call_bindings: crate::types::TyParamBindings,
}

pub(super) struct GenericMemberQuery<'a> {
    pub(super) owner: Option<TypeName>,
    pub(super) params: Option<&'a [Ty]>,
    pub(super) partial_args: Option<&'a [Option<Ty>]>,
    pub(super) partial_arg_names: Option<&'a [Option<String>]>,
    pub(super) explicit_type_args: &'a [Ty],
    pub(super) expected_result: Option<Ty>,
}

pub(super) fn generic_member_lambda_params(
    source: &dyn SymbolSource,
    plan: &GenericMemberPlan,
    parameter: usize,
) -> Option<Vec<Ty>> {
    let shape = *plan.method.signature.params.get(parameter)?;
    let target = apply_inference_bindings(shape, &plan.call_bindings);
    match target.non_null().kind() {
        TyKind::Fun(signature) => Some(signature.params.clone()),
        _ => crate::symbol_resolver::semantic_sam_signature(source, target).map(|sam| sam.params),
    }
}

pub(super) fn module_member_lambda_params(
    source: &dyn SymbolSource,
    member: &crate::libraries::LibraryMember,
    parameter: usize,
) -> Option<Vec<Ty>> {
    let param = *member.params.get(parameter)?;
    match param.kind() {
        TyKind::Fun(function) => Some(
            member
                .call_sig
                .lambda_param_types
                .get(parameter)
                .cloned()
                .unwrap_or_else(|| function.params.clone()),
        ),
        // A functional-interface parameter contributes its single abstract method's value-parameter
        // shape. `LibraryMember` is already the federated semantic form, so declaration origin does
        // not enter this fallback.
        _ => crate::symbol_resolver::semantic_sam_signature(source, param).map(|sam| sam.params),
    }
}

impl ClassSig {
    pub(super) fn set_inferred_source_method_return(
        &mut self,
        source_member: crate::libraries::SourceMember,
        ret: Ty,
    ) -> bool {
        self.source_methods
            .iter_mut()
            .find(|plan| plan.signature.source_member == Some(source_member))
            .is_some_and(|plan| plan.signature.set_inferred_return(ret))
    }

    pub(super) fn set_inferred_method_return(
        &mut self,
        name: &str,
        params: &[Ty],
        ret: Ty,
    ) -> bool {
        let ret = inferred_declaration_ty(ret);
        let mut changed = false;
        if let Some(signature) = self.methods.get_mut(name).and_then(|overloads| {
            overloads
                .iter_mut()
                .find(|signature| same_erased_params(&signature.params, params))
        }) {
            changed |= signature.set_inferred_return(ret);
        }
        if let Some(method) = self.generic_methods.get_mut(name).and_then(|overloads| {
            overloads
                .iter_mut()
                .find(|method| same_erased_params(&method.params, params))
        }) {
            changed |= method.signature.ret != ret;
            method.signature.ret = ret;
        }
        changed
    }

    pub(super) fn set_inferred_member_extension_return(
        &mut self,
        name: &str,
        receiver: Ty,
        params: &[Ty],
        ret: Ty,
    ) -> bool {
        self.member_ext_funs
            .get_mut(name)
            .and_then(|overloads| {
                overloads.iter_mut().find(|function| {
                    erased_type_key(function.receiver_ty) == erased_type_key(receiver)
                        && same_erased_params(&function.signature.params, params)
                })
            })
            .is_some_and(|function| function.signature.set_inferred_return(ret))
    }

    /// True if this is an `interface` (calls dispatch via `invokeinterface`).
    #[inline]
    pub fn is_interface(&self) -> bool {
        self.flags.has(ClassFlags::INTERFACE)
    }
    /// True for an `enum class`.
    #[inline]
    pub fn is_enum(&self) -> bool {
        self.flags.has(ClassFlags::ENUM)
    }
    /// Record a declared enum entry once, keeping source order.
    pub(crate) fn declare_enum_entry(&mut self, name: &str) {
        if !self.enum_entries.iter().any(|entry| entry == name) {
            self.enum_entries.push(name.to_owned());
        }
    }
    /// True if declared `object` (a singleton).
    #[inline]
    pub fn is_object(&self) -> bool {
        self.flags.has(ClassFlags::OBJECT)
    }
    /// True if declared `abstract` (or `sealed`, which is abstract) — cannot be instantiated directly.
    #[inline]
    pub fn is_abstract(&self) -> bool {
        self.flags.has(ClassFlags::ABSTRACT)
    }
    /// True if declared `fun interface` — a single-abstract-method interface eligible for SAM conversion.
    #[inline]
    pub fn is_fun_interface(&self) -> bool {
        self.flags.has(ClassFlags::FUN_INTERFACE)
    }
    /// True if declared `sealed` — all subclasses are known in this module.
    #[inline]
    pub fn is_sealed(&self) -> bool {
        self.flags.has(ClassFlags::SEALED)
    }
    /// True when the declaration cannot be subclassed.
    #[inline]
    pub fn is_final(&self) -> bool {
        self.flags.has(ClassFlags::FINAL)
    }
    /// True when the type declares one or more abstract members.
    #[inline]
    pub fn has_abstract_members(&self) -> bool {
        self.flags.has(ClassFlags::HAS_ABSTRACT_MEMBERS)
    }
    /// True for an `annotation class` — emitted as an interface; instantiation builds a synthetic impl.
    #[inline]
    pub fn is_annotation(&self) -> bool {
        self.flags.has(ClassFlags::ANNOTATION)
    }
    /// True for a Kotlin `data class`.
    #[inline]
    pub fn is_data(&self) -> bool {
        self.flags.has(ClassFlags::DATA)
    }
    #[inline]
    pub fn set_is_abstract(&mut self, on: bool) {
        self.flags = self.flags.with_abstract(on);
    }
    #[inline]
    pub fn set_is_sealed(&mut self, on: bool) {
        self.flags = self.flags.with_sealed(on);
    }
    #[inline]
    pub fn set_is_final(&mut self, on: bool) {
        self.flags = self.flags.with_final(on);
    }

    pub(crate) fn type_parameter_bindings(&self, applied: Ty) -> crate::types::TyParamBindings {
        let mut bindings = self
            .type_params
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let bound = self
                    .type_param_bounds
                    .get(index)
                    .copied()
                    .filter(|bound| *bound != Ty::Error)
                    .unwrap_or_else(|| Ty::obj("kotlin/Any"));
                (crate::types::TyParamName::from(name), bound)
            })
            .collect::<HashMap<_, _>>();
        for (name, argument) in self.type_params.iter().zip(applied.type_args()) {
            bindings.insert(crate::types::TyParamName::from(name), *argument);
        }
        bindings.extend(
            self.captured_type_parameters
                .type_params
                .iter()
                .enumerate()
                .map(|(index, name)| {
                    let bound = self
                        .captured_type_parameters
                        .type_param_bounds
                        .get(index)
                        .copied()
                        .filter(|bound| *bound != Ty::Error)
                        .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
                    (crate::types::TyParamName::from(name), bound)
                }),
        );
        for (name, argument) in self
            .captured_type_parameters
            .type_params
            .iter()
            .zip(applied.type_args().iter().skip(self.type_params.len()))
        {
            bindings.insert(crate::types::TyParamName::from(name), *argument);
        }
        bindings
    }

    pub(crate) fn has_no_arg_constructor(&self) -> bool {
        (self.has_primary_ctor
            && (self.ctor_params.is_empty()
                || (self.ctor_param_names.len() == self.ctor_params.len()
                    && self
                        .ctor_param_names
                        .iter()
                        .all(|(_, has_default)| *has_default))))
            || self.secondary_ctors.iter().any(Vec::is_empty)
    }

    pub fn internal_name(&self) -> TypeName {
        self.internal
    }

    pub fn internal(&self) -> String {
        self.internal.render()
    }

    pub fn internal_matches(&self, internal: &str) -> bool {
        self.internal.matches(internal)
    }

    pub fn inner_of_name(&self) -> Option<TypeName> {
        self.inner_of
    }

    pub fn inner_of_matches(&self, internal: &str) -> bool {
        self.inner_of.is_some_and(|outer| outer.matches(internal))
    }

    pub fn super_internal_name(&self) -> Option<TypeName> {
        self.super_internal
    }

    pub fn interface_names(&self) -> impl Iterator<Item = TypeName> + '_ {
        self.interfaces.iter_ids()
    }

    pub fn prop(&self, name: &str) -> Option<(Ty, bool)> {
        if let Some(property) = self.declared_props.get(name) {
            return property
                .context_params
                .is_empty()
                .then_some((property.ty, property.setter_name.is_some()));
        }
        self.props
            .iter()
            .find_map(|(n, t, v)| (n == name).then_some((*t, *v)))
    }

    /// Stable identity of a declared property while both the source declaration inventory and the
    /// compact declaration projection are live. Exact transient identity wins; the name fallback
    /// covers the compact projection, which deliberately carries no parser coordinate.
    pub fn stable_property_declaration(
        &self,
        source: crate::libraries::SourceMember,
        name: &str,
    ) -> Option<crate::fir::DeclarationId> {
        self.declared_props
            .values()
            .chain(self.contextual_props.values().flatten())
            .find(|property| property.source_member == Some(source))
            .or_else(|| self.declared_props.get(name))
            .and_then(|property| property.stable_declaration)
    }

    /// A property visible to Kotlin source lookup. Internal capture storage deliberately remains
    /// available through [`Self::prop`] for bounded classifier checking, while this operation
    /// excludes it and lets an inherited declaration with the same spelling remain visible.
    pub fn source_prop(&self, name: &str) -> Option<(Ty, bool)> {
        if let Some(property) = self.declared_props.get(name) {
            return (property.source_visible && property.context_params.is_empty())
                .then_some((property.ty, property.setter_name.is_some()));
        }
        self.props
            .iter()
            .find_map(|(n, t, v)| (n == name).then_some((*t, *v)))
    }

    pub fn member_ext_props(&self, name: &str) -> &[MemberExtPropSig] {
        self.member_ext_props.get(name).map_or(&[], Vec::as_slice)
    }

    pub fn contextual_props(&self, name: &str) -> &[DeclaredPropertySig] {
        self.contextual_props.get(name).map_or(&[], Vec::as_slice)
    }

    pub fn member_ext_funs(&self, name: &str) -> &[MemberExtFunSig] {
        self.member_ext_funs.get(name).map_or(&[], Vec::as_slice)
    }

    /// The SOLE signature declared under `name` — `None` when the name is absent OR overloaded
    /// (an overloaded name needs [`Self::method_matching`] with the call's argument types; callers
    /// without arguments in hand must not guess an overload).
    pub fn method(&self, name: &str) -> Option<&Signature> {
        match self.methods.get(name)?.as_slice() {
            [one] => Some(one),
            _ => None,
        }
    }

    /// The overload of `name` matching `args` (same arity/assignability scoring as top-level
    /// [`pick_overload`]); ambiguity resolves to `None` so the caller skips rather than misdispatches.
    pub fn method_matching(&self, name: &str, args: &[Ty]) -> Option<&Signature> {
        let sigs = self.methods.get(name)?;
        pick_overload(sigs, args).map(|i| &sigs[i])
    }

    /// All signatures declared under `name`, in declaration order (empty slice if none).
    pub fn methods_named(&self, name: &str) -> &[Signature] {
        self.methods.get(name).map_or(&[], Vec::as_slice)
    }

    pub fn has_method(&self, name: &str) -> bool {
        self.methods.get(name).is_some_and(|v| !v.is_empty())
    }

    /// Append an overload under `name` (declaration order preserved per name).
    pub fn add_method(&mut self, name: String, sig: Signature) {
        if !self.methods.contains_key(&name) {
            self.declared_callable_order.push(name.clone());
        }
        self.methods.entry(name).or_default().push(sig);
    }
}

/// Per-name overload lists (declaration order within a name). One `Vec` per name keeps the common
/// single-overload case cheap while letting member overloading resolve by argument types.
pub type MethodMap = HashMap<String, Vec<Signature>>;
