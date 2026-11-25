use super::*;

/// A collected TOP-LEVEL extension property: its declared type, mutability, and the source
/// declaration key (file index, decl id) — how the backend's facade-metadata builder matches a
/// declaration to its record without guessing by name (same-name properties may differ by receiver).
#[derive(Clone, Debug)]
pub struct ExtPropSig {
    /// Source-declared names retained only for metadata/display.
    pub formal_names: Vec<String>,
    /// Stable semantic identities used inside receiver/result types and generic inference.
    pub formals: Vec<crate::types::TyParamName>,
    pub formal_bounds: Vec<Ty>,
    pub receiver: Ty,
    pub ty: Ty,
    pub is_var: bool,
    pub is_companion_extension: bool,
    pub is_companion_block_member: bool,
    pub getter_name: String,
    pub setter_name: Option<String>,
    pub context_params: Vec<Ty>,
    pub accepts_nullable_receiver: bool,
    pub source: (u32, u32),
    pub package: TypeName,
    pub visibility: Visibility,
    /// Resolved declaration annotation identities. These are semantic header metadata; Pass 2
    /// must not recover them from the original annotation syntax or source coordinates.
    pub annotations: Vec<TypeName>,
    pub stable_declaration: Option<crate::fir::DeclarationId>,
}

impl ExtPropSig {
    pub(crate) fn generic_signature(&self) -> Option<GenericSig> {
        (!self.formals.is_empty()).then(|| GenericSig {
            formals: self.formals.clone(),
            formal_bounds: self
                .formal_bounds
                .iter()
                .copied()
                .map(|bound| vec![bound])
                .collect(),
            receiver: Some(self.receiver),
            params: self.context_params.clone(),
            ret: self.ty,
            return_policy: Default::default(),
        })
    }
}

/// Whether two generic extension-property receivers impose the same declaration-site constraints.
/// Their semantic type-parameter identities are deliberately declaration-unique, so compare after
/// alpha-renaming the left declaration onto the right declaration's formals.
pub(super) fn extension_property_headers_alpha_equivalent(
    left: &ExtPropSig,
    right: &ExtPropSig,
) -> bool {
    if left.formals.len() != right.formals.len()
        || left.formal_bounds.len() != right.formal_bounds.len()
    {
        return false;
    }
    let placeholders = left
        .formals
        .iter()
        .zip(right.formals.iter().zip(&right.formal_bounds))
        .map(|(left, (right, bound))| {
            (
                crate::types::TyParamName::from(left),
                Ty::ty_param(right, *bound),
            )
        })
        .collect::<crate::symbol_resolver::GSigBinds>();
    crate::symbol_resolver::ty_subst_keep_unbound(left.receiver, &placeholders) == right.receiver
        && left
            .formal_bounds
            .iter()
            .copied()
            .map(|bound| crate::symbol_resolver::ty_subst_keep_unbound(bound, &placeholders))
            .eq(right.formal_bounds.iter().copied())
}

pub struct SourcePropertySig {
    pub name: String,
    /// Semantic identities of the property's own type parameters. Context arguments and an
    /// extension receiver may both constrain these at a read/write site.
    pub formals: Vec<crate::types::TyParamName>,
    /// Declared upper bound parallel to `formals`. The stable signature bridge uses these semantic
    /// identities while resolving the property's compact context/result header; retaining only the
    /// source spelling (`T`) would make a generic property a different declaration in Pass 1 and
    /// Pass 2.
    pub formal_bounds: Vec<Ty>,
    /// Declared property type published in metadata and by its accessor signature.
    pub ty: Ty,
    /// Narrower explicit backing-field type. A final source property may expose this as its stable
    /// read type where the declaration body is available, while its getter keeps returning `ty`.
    pub storage_ty: Option<Ty>,
    pub is_var: bool,
    pub is_const: bool,
    /// Whether this declaration uses Kotlin's `lateinit` storage contract. This is a stable header
    /// fact used by `::property.isInitialized`; no body or initializer syntax is retained for it.
    pub is_lateinit: bool,
    /// The checked initializer's non-null compile-time value. Lowering and metadata emission consume
    /// this semantic fact instead of independently re-reading initializer syntax.
    pub compile_time_constant: Option<crate::libraries::LibraryConst>,
    pub implicit_integer_coercion: bool,
    pub context_params: Vec<Ty>,
    /// Source names parallel to `context_params`, retained for diagnostics after resolution.
    pub context_param_names: Vec<String>,
    /// Typed identities parallel to `context_params`, captured while source syntax is live.
    pub context_parameter_identities: Vec<crate::fir::ResolvedParameterIdentity>,
    pub package: TypeName,
    pub visibility: Visibility,
    pub setter_visibility: Visibility,
    pub setter_parameter_name: Option<String>,
    pub read_stability: crate::libraries::PropertyReadStability,
    /// Resolved declaration annotation identities projected into the finalized declaration header.
    pub annotations: Vec<TypeName>,
    pub stable_declaration: Option<crate::fir::DeclarationId>,
}

#[derive(Clone, Debug)]
pub struct ResolvedPropertyAccess {
    pub property: crate::libraries::PropertyInfo,
    pub context_args: Vec<ResolvedContextArgument>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct MissingContextParameter {
    pub(super) index: usize,
    pub(super) ty: Ty,
}

impl MissingContextParameter {
    pub(super) fn display(self, names: &[String]) -> String {
        names
            .get(self.index)
            .filter(|name| !name.is_empty() && name.as_str() != "_")
            .map_or_else(
                || self.ty.source_name(),
                |name| format!("{name}: {}", self.ty.source_name()),
            )
    }
}

pub(super) enum TopLevelPropertySelection {
    None,
    Selected(Box<ResolvedPropertyAccess>),
    MissingContext(MissingContextParameter, Vec<String>),
    Ambiguous,
}

type GenericMemberValueOperandShape = (Option<Ty>, Vec<Ty>, Vec<u32>);
type GenericMemberValueOperandSlots =
    HashMap<TypeName, HashMap<String, Vec<GenericMemberValueOperandShape>>>;

pub(super) enum PropertyReadSelection {
    Member(Box<PropertyReadMemberSelection>),
    MemberExtension(Box<MemberExtensionProperty>),
    /// The selected extension, with the actual receiver when it mentions a postponed call's type
    /// variables: reading the property adds that receiver constraint to the call.
    Extension(Box<ResolvedPropertyAccess>, Option<Ty>),
}

type ModuleSymbolCache = HashMap<
    (Option<u32>, crate::symbol_source::SymbolNamespace),
    HashMap<String, std::rc::Rc<crate::libraries::ResolvedSymbols>>,
>;

#[derive(Clone)]
pub(super) enum PropertyInferenceFailure {
    Recursive { span: Span },
    UninitializedReads(Vec<(Span, String)>),
    Untypeable,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct VisibilitySuppressions {
    pub(super) invisible_reference: bool,
    pub(super) invisible_member: bool,
    pub(super) optional_declaration_usage: bool,
}

impl VisibilitySuppressions {
    fn bypasses_visibility(self) -> bool {
        self.invisible_reference || self.invisible_member
    }

    pub(super) fn has_any(self) -> bool {
        self.bypasses_visibility() || self.optional_declaration_usage
    }
}

pub struct SymbolTable {
    /// Opaque identity of this compilation. Declaration-owned generic variables include it so two
    /// concurrent modules with identical file offsets cannot alias in the process-wide type interner.
    pub(super) compilation_id: u64,
    /// Module-owned declaration-query cache. It is never shared with another compilation session;
    /// dependency providers own their caches separately. The optional file component preserves
    /// private top-level visibility.
    pub(crate) module_symbol_cache: std::cell::RefCell<ModuleSymbolCache>,
    pub(crate) source_fragments: crate::source::SourceFragments,
    pub(crate) module_shape_cache: std::cell::RefCell<
        HashMap<TypeName, Option<std::sync::Arc<crate::libraries::LibraryType>>>,
    >,
    pub(super) module_cache_enabled: std::cell::Cell<bool>,
    /// How many mutation brackets are open — see [`SymbolTable::begin_module_mutation`].
    pub(super) module_mutation_depth: std::cell::Cell<usize>,
    pub funs: HashMap<String, Vec<Signature>>,
    pub(super) conflicting_top_level_keys: crate::fx_hash::HashSet<TopLevelFunctionConflictKey>,
    pub(super) conflicting_top_level_key_by_source:
        HashMap<(u32, u32), TopLevelFunctionConflictKey>,
    pub(super) conflicting_top_level_candidates:
        HashMap<TopLevelFunctionConflictKey, TopLevelFunctionConflictCandidates>,
    /// Declared classes by JVM internal name (e.g. `pkg/Point`, `pkg/Outer$Inner`) — two classes
    /// sharing a SIMPLE name in different packages are distinct entries, so member lookup on one
    /// never evicts the other. This is a strict invariant: source aliases live in the separate
    /// declaration-keyed alias index below and never introduce a differently shaped key here.
    pub classes: HashMap<TypeName, ClassSig>,
    /// Authoritative package namespace declared by the source files, including ancestor packages.
    /// Signature collection seeds the complete set before it queries provisional declarations, so
    /// package lookup stays O(1) while the class/callable tables continue to mutate.
    pub(crate) source_packages: crate::fx_hash::HashSet<TypeName>,
    /// Declaration-header facts keyed by classifier identity, available before full signatures are
    /// collected. Forward property inference may need to classify a later-file source singleton; this
    /// identity table avoids both source-order dependence and the old global simple-name object set.
    pub(super) source_class_headers: HashMap<TypeName, SourceClassHeader>,
    /// Every declared upper bound of each source type parameter that has several, keyed by its
    /// semantic identity. `Ty::TyParam` carries only one; assignability reads all of them here.
    /// Classifier parameters are recorded as their classifier is inserted, callable parameters by
    /// [`SymbolTable::index_callable_type_parameter_bounds`] once collection is done.
    pub(super) declared_type_parameter_bounds: type_parameter_bounds::DeclaredBoundsIndex,
    /// Parser classifier coordinates retained only while Pass 1 inspection/capture analysis is
    /// active, bound once to the compact header's stable semantic identity. This prevents that
    /// legacy retained-AST lane from rebuilding an anonymous/local classifier from its parser
    /// placeholder spelling after compact signature collection has assigned the real identity.
    pub(super) stable_parser_classifier_identities: HashMap<(u32, DeclId), TypeName>,
    /// Source `typealias` bindings by declaring file and alias spelling. An alias is a name-resolution
    /// edge to a classifier identity, not another class declaration; keeping it separate prevents a
    /// simple alias key from corrupting the internal-name invariant of [`Self::classes`].
    pub(super) source_class_aliases: HashMap<(u32, String), TypeName>,
    /// The same alias edges keyed by FULLY-QUALIFIED name (`pkg/Alias` → the target's internal name).
    /// The per-file index above answers a spelling inside its declaring file; a qualified reference
    /// (`pkg.Alias(…)`) names the alias from anywhere and has no file to key on, so the declaring
    /// file's package is folded into the key here. Kept beside the per-file map rather than in
    /// [`Self::classes`], whose keys must stay real classifier identities.
    pub source_alias_fqns: HashMap<TypeName, TypeName>,
    /// `(owner classifier, alias name)` → the qualified identity of a typealias declared in that
    /// classifier's body, as its publication assigned it.
    pub nested_type_aliases: HashMap<(TypeName, String), TypeName>,
    /// Source `typealias` EXPANSIONS by fully-qualified alias name: the alias's own formal names
    /// and its target applied to its own arguments, with those formals as `Ty::TyParam`
    /// (`typealias Box<S, A> = PBox<S, S, A, A>`). The name edge above records only the target
    /// CLASSIFIER, which is all name resolution needs; `@Metadata` must describe the expansion so a
    /// consumer can place a use site's arguments. Every valid source alias has one; an unresolved
    /// declaration is a frontend error and compilation stops before emission.
    pub source_alias_expansions: HashMap<TypeName, crate::libraries::TypeAliasTemplate>,
    /// Source SPELLINGS of declared types, keyed by stable Pass-1 identity — see
    /// [`crate::spelling`]. `@Metadata` records a `typealias` named in a declared type as
    /// `Type.abbreviated_type`, and `Ty` is fully expanded by the time the builders see it, so the
    /// spelling travels beside it rather than inside it. The compact header inventory is the only
    /// producer; a declaration-only support source contributes no declared spellings.
    pub stable_declared_spellings:
        HashMap<crate::fir::DeclarationId, crate::spelling::DeclaredSpellings>,
    /// Source spans of checked type-use annotations while Pass 1 still owns their occurrences,
    /// keyed by stable declaration, expanded semantic type path, and resolved annotation identity.
    /// Constructor-plan diagnostics consume this before `SymbolTable` is destroyed; no coordinate
    /// crosses into checked FIR or common IR.
    pub(crate) type_use_annotation_spans:
        HashMap<(crate::fir::DeclarationId, Box<[u32]>, TypeName), Span>,
    /// How each source `typealias` spelled its own RIGHT-HAND SIDE, keyed by the alias's identity.
    ///
    /// Two things need it. `TypeAlias.underlying_type` (f4) names an alias DIRECTLY when the
    /// right-hand side spells one (`typealias Chain = Cargo` writes `type_alias_name = Cargo`, not
    /// the expanded class). And a use site of that alias inherits the spelling into its EXPANSION:
    /// `typealias CargoBox = PBox<Cargo, Cargo>` abbreviates both expanded arguments as `Cargo`.
    ///
    /// The value is the right-hand side's spelling, the alias's formal parameter names, and its
    /// expansion type — the last two are what let a use site push its OWN argument spellings into
    /// the parameter positions of the expansion (`typealias Boxed<T> = PBox<T, T>` used as
    /// `Boxed<Cargo>` abbreviates both expanded arguments as `Cargo`).
    pub alias_expansion_spellings: HashMap<TypeName, (crate::spelling::Spelled, Vec<String>, Ty)>,
    pub(super) anonymous_object_types: HashMap<(u32, DeclId), TypeName>,
    pub(super) anonymous_object_captures: HashMap<(u32, DeclId), Vec<AnonymousObjectCapture>>,
    pub(super) anonymous_object_capture_discovered: crate::fx_hash::HashSet<(u32, DeclId)>,
    /// Top-level properties (name → type, is_var, is_const), backed by static fields on the file facade.
    /// `is_const` distinguishes a `const val` (public field, no accessor, cross-file `getstatic`) from a
    /// plain `val`/`var` (private field, read/written through `getX`/`setX`).
    pub props: HashMap<String, (Ty, bool, bool)>,
    /// Top-level properties keyed by source declaration. Context parameters are ordinary leading
    /// accessor parameters; an empty list is the regular property case.
    pub source_props: HashMap<(u32, u32), SourcePropertySig>,
    pub context_prop_names: crate::fx_hash::HashSet<String>,
    /// Why an implicitly typed property could not be resolved, keyed by its source declaration.
    /// The authoritative checker consumes this fact once; reads keep the ordinary error type and
    /// do not reconstruct the failure from a spelling or emit cascades.
    pub(super) property_inference_failures: HashMap<DeclKey, PropertyInferenceFailure>,
    /// `(source file, declaration)` → the `@JvmName` spelling of a top-level function whose emitted
    /// method name differs from its source name. A CROSS-FILE caller resolves the callee by source
    /// name but must emit the annotated one, and the callee's AST is out of its reach — so the name
    /// is recorded here, where the collect pass can see every file. Absent = the two agree.
    pub toplevel_jvm_names: HashMap<(u32, u32), String>,
    /// The target's compiled library set — a JVM classpath or a klib (empty unless the driver
    /// supplies one). The front end resolves external references only through this abstraction.
    pub libraries: Box<dyn SemanticPlatform>,
    /// The native compiler plugins this compilation runs (none unless the driver selects them).
    pub(super) native_plugins: crate::plugins::registry::NativePlugins,
    /// The annotation-configured conventions of the native plugins this compilation runs
    /// ([`crate::symbol_source::SymbolSource::annotation_conventions`]).
    pub(crate) annotation_conventions: crate::symbol_source::AnnotationConventions,
    /// Top-level extension overloads keyed by name and semantic receiver.
    pub ext_funs: HashMap<String, HashMap<Ty, Vec<Signature>>>,
    pub(super) source_ext_funs: HashMap<(u32, u32), (String, Ty, usize)>,
    /// Top-level extension properties: (erased receiver, prop_name) → (type, is_var). The
    /// getter/setter are emitted as static `getName(Recv)`/`setName(Recv, T)` methods.
    pub ext_props: HashMap<(Ty, String), Vec<ExtPropSig>>,
    /// Simple type name → JVM internal name: every resolvable reference type — user/classpath
    /// classes, classpath `TypeAliasesKt` aliases, and the ported `JavaToKotlinClassMap`
    /// built-ins. The single source of truth for "does this type name resolve, and to what".
    pub class_names: ClassNames,
    /// Temporary Pass-1 bindings from annotation occurrences to resolved classifier identities.
    /// Signature finalization projects declaration-owned facts to stable declaration ids, then
    /// production releases this coordinate map before Pass 2.
    pub(super) resolved_annotations: BoundAnnotations,
    /// Visibility diagnostics explicitly suppressed on a stable declaration. This is compact
    /// resolved annotation state, not retained annotation syntax or a parser-id side table.
    pub(super) declaration_visibility_suppressions:
        HashMap<crate::fir::DeclarationId, VisibilitySuppressions>,
    /// Retention normalized at the declaration/provider boundary by resolved annotation identity.
    /// Lowering and backends consume this fact without reopening a source declaration or classpath.
    pub(super) annotation_retentions: HashMap<TypeName, crate::types::AnnotationRetention>,
    /// Declared `@Target` policy per annotation identity: a library's from its provider, a source
    /// class's from its checked `@Target` application. An identity with no valid policy is
    /// applicable everywhere ([`crate::types::AnnotationTargets::DEFAULT`]).
    pub(super) annotation_targets: crate::types::DeclaredTargetPolicies,
    /// Top-level function name → the facade class it lives on (`helper` → `pkg/AKt`), for the WHOLE
    /// multi-file compilation. Populated only by the multi-file driver (which knows each file's
    /// stem/facade); empty for single-file/in-process callers. Lets `lower_file` emit a call to a
    /// function defined in ANOTHER file as a cross-facade `invokestatic` (`Callee::CrossFile`) instead
    /// of bailing. A function defined in the file being lowered is resolved locally first.
    pub fn_facades: HashMap<String, TypeName>,
    /// Top-level function source declaration → declaring facade. This is the declaration-keyed
    /// equivalent of [`Self::fn_facades`], used once the checker has selected a concrete overload.
    pub fn_facades_by_decl: HashMap<(u32, u32), TypeName>,
    /// Source functions the JVM module-registration pass deliberately omitted from
    /// [`Self::fn_facades_by_decl`]. An absent positive entry is otherwise ambiguous: it can mean
    /// either "splice-only, never emitted" or simply "this checker-only pipeline did not run JVM
    /// registration". Keeping the authoritative negative result beside the positive map lets the
    /// checker diagnose only the former without reimplementing backend emission policy or accepting
    /// a facade class name leaked through a generic symbol origin.
    pub(super) unemitted_fn_facades_by_decl: crate::fx_hash::HashSet<(u32, u32)>,
    /// Bare generic value operands keyed by source declaration.
    pub source_generic_value_operand_slots: HashMap<(u32, u32), Vec<u32>>,
    /// Bare generic value operands for source members.
    pub(crate) source_generic_member_value_operand_slots: GenericMemberValueOperandSlots,
    pub source_projected_return_hazards: crate::fx_hash::HashSet<(u32, u32)>,
    pub ext_prop_facades_by_decl: HashMap<(u32, u32), TypeName>,
    /// Physical facade selected for every top-level source property. Semantic property resolution is
    /// declaration-keyed; the JVM backend fills this map so cross-file accessor emission never has to
    /// derive an owner from a property name.
    pub prop_facades_by_decl: HashMap<(u32, u32), TypeName>,
    /// Top-level property name → `(facade_internal, type, is_var)` across the WHOLE multi-file
    /// compilation. Populated only by the multi-file driver. A read of a property from ANOTHER file
    /// lowers to `invokestatic <facade>.getX()` (the field is private), a write to `setX(v)`. Empty for
    /// single-file callers; a property in the file being lowered is resolved locally (its static) first.
    pub prop_facades: HashMap<String, (TypeName, Ty, bool, bool)>,
}

/// The complete semantic environment that is allowed to survive the Pass-1/Pass-2 boundary.
///
/// Current-module declarations live exclusively in `ResolvedModuleIndex` after signature
/// finalization.  This type therefore cannot contain a parser-keyed declaration map, signature
/// graph, source spelling sidecar, or target-preparation table.  Consuming a [`SymbolTable`] into
/// this value physically destroys all of those Pass-1 structures before ordinary bodies are
/// reparsed.
pub(crate) struct PassTwoSymbols {
    pub(super) compilation_id: u64,
    pub(super) libraries: std::rc::Rc<dyn SemanticPlatform>,
    coroutine_runtime: Option<crate::ir::IrCoroutineRuntime>,
    char_sequence_protocol: Option<crate::fir::CharSequenceProtocol>,
    enum_entries_factory: Option<crate::fir::EnumEntriesFactory>,
    pub(super) native_plugins: crate::plugins::registry::NativePlugins,
}

impl PassTwoSymbols {
    pub(crate) fn semantic_platform(&self) -> &dyn SemanticPlatform {
        &*self.libraries
    }

    /// A shared handle on the platform, for a consumer that outlives one borrow of these symbols.
    pub(crate) fn shared_semantic_platform(&self) -> std::rc::Rc<dyn SemanticPlatform> {
        std::rc::Rc::clone(&self.libraries)
    }

    /// The runtime declarations selected by resolution for suspend lowering.
    pub(crate) fn coroutine_runtime(&self) -> Option<crate::ir::IrCoroutineRuntime> {
        self.coroutine_runtime
    }

    /// The `CharSequence` members resolution selected for a builder that reads an appended
    /// sequence through them.
    pub(crate) fn char_sequence_protocol(&self) -> Option<crate::fir::CharSequenceProtocol> {
        self.char_sequence_protocol
    }

    /// The `kotlin.enums.enumEntries(Array)` resolution selected for a target that builds an
    /// enum's `entries` by it.
    pub(crate) fn enum_entries_factory(&self) -> Option<crate::fir::EnumEntriesFactory> {
        self.enum_entries_factory
    }

    pub(crate) fn native_plugins(&self) -> &crate::plugins::registry::NativePlugins {
        &self.native_plugins
    }
}

impl SymbolTable {
    pub(crate) fn into_pass_two_symbols(self) -> PassTwoSymbols {
        let coroutine_runtime = coroutine_runtime::selected(&*self.libraries);
        let char_sequence_protocol = char_sequence_protocol::selected(&*self.libraries);
        let enum_entries_factory = enum_entries_factory::selected(&*self.libraries);
        PassTwoSymbols {
            compilation_id: self.compilation_id,
            libraries: std::rc::Rc::from(self.libraries),
            coroutine_runtime,
            char_sequence_protocol,
            enum_entries_factory,
            native_plugins: self.native_plugins,
        }
    }
}

impl Default for SymbolTable {
    fn default() -> SymbolTable {
        static NEXT_COMPILATION_ID: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(1);
        SymbolTable {
            compilation_id: NEXT_COMPILATION_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            module_symbol_cache: Default::default(),
            source_fragments: Default::default(),
            module_shape_cache: Default::default(),
            module_cache_enabled: std::cell::Cell::new(false),
            module_mutation_depth: std::cell::Cell::new(0),
            funs: HashMap::default(),
            conflicting_top_level_keys: crate::fx_hash::HashSet::default(),
            conflicting_top_level_key_by_source: HashMap::default(),
            conflicting_top_level_candidates: HashMap::default(),
            classes: HashMap::default(),
            source_packages: crate::fx_hash::HashSet::default(),
            source_class_headers: HashMap::default(),
            declared_type_parameter_bounds: type_parameter_bounds::DeclaredBoundsIndex::default(),
            stable_parser_classifier_identities: HashMap::default(),
            source_class_aliases: HashMap::default(),
            source_alias_fqns: HashMap::default(),
            nested_type_aliases: HashMap::default(),
            source_alias_expansions: HashMap::default(),
            stable_declared_spellings: HashMap::default(),
            type_use_annotation_spans: HashMap::default(),
            alias_expansion_spellings: HashMap::default(),
            anonymous_object_types: HashMap::default(),
            anonymous_object_captures: HashMap::default(),
            anonymous_object_capture_discovered: crate::fx_hash::HashSet::default(),
            props: HashMap::default(),
            source_props: HashMap::default(),
            context_prop_names: crate::fx_hash::HashSet::default(),
            property_inference_failures: HashMap::default(),
            toplevel_jvm_names: HashMap::default(),
            libraries: Box::new(EmptySymbolSource),
            native_plugins: Default::default(),
            annotation_conventions: Default::default(),
            ext_funs: HashMap::default(),
            source_ext_funs: HashMap::default(),
            ext_props: HashMap::default(),
            class_names: ClassNames::default(),
            resolved_annotations: HashMap::default(),
            declaration_visibility_suppressions: HashMap::default(),
            annotation_retentions: HashMap::default(),
            annotation_targets: crate::types::DeclaredTargetPolicies::default(),
            fn_facades: HashMap::default(),
            fn_facades_by_decl: HashMap::default(),
            unemitted_fn_facades_by_decl: crate::fx_hash::HashSet::default(),
            source_generic_value_operand_slots: HashMap::default(),
            source_generic_member_value_operand_slots: HashMap::default(),
            source_projected_return_hazards: crate::fx_hash::HashSet::default(),
            ext_prop_facades_by_decl: HashMap::default(),
            prop_facades_by_decl: HashMap::default(),
            prop_facades: HashMap::default(),
        }
    }
}

impl SymbolTable {
    /// Whether this stable source classifier is an annotation expectation whose absence on the
    /// current target is explicitly permitted. The marker is compared by its resolved classifier
    /// identity, so import aliases and same-named source declarations cannot change the answer.
    pub(crate) fn is_source_optional_expectation(
        &self,
        declaration: crate::fir::DeclarationId,
    ) -> bool {
        self.classes.values().any(|class| {
            class.stable_declaration == Some(declaration)
                && class.is_annotation()
                && class
                    .annotations
                    .contains(&type_name("kotlin/OptionalExpectation"))
        })
    }

    pub(crate) fn module_cache_enabled(&self) -> bool {
        self.module_cache_enabled.get()
    }

    /// Signature collection mutates the module while querying provisional declarations, so caching
    /// is disabled there. A checker enables the cache only after its pre-inference mutation phase.
    /// A mutation bracket NESTS: a pass that brackets its whole body can call another that brackets
    /// its own, and the inner `finish` must not re-enable the cache while the outer pass is still
    /// writing. The depth counter is what makes the pair safe to compose; a bare flag turned the
    /// inner `finish` into an outer one and let the second half of the outer pass read a stale
    /// module cache.
    pub(super) fn begin_module_mutation(&self) {
        self.module_mutation_depth
            .set(self.module_mutation_depth.get() + 1);
        self.module_cache_enabled.set(false);
        self.clear_module_symbol_cache();
    }

    pub(crate) fn finish_module_mutation(&self) {
        let depth = self.module_mutation_depth.get().saturating_sub(1);
        self.module_mutation_depth.set(depth);
        self.clear_module_symbol_cache();
        if depth == 0 {
            self.module_cache_enabled.set(true);
        }
    }

    fn clear_module_symbol_cache(&self) {
        self.module_symbol_cache.borrow_mut().clear();
        self.module_shape_cache.borrow_mut().clear();
    }

    fn register_source_package_for_owner(&mut self, owner: TypeName) {
        let mut package = owner.parent();
        while let Some(current) = package {
            if current == type_name("") || !self.source_packages.insert(current) {
                break;
            }
            package = current.parent();
        }
    }
    /// Record the JVM module registrar's complete answer for one source function. `Some` is the
    /// concrete facade static the lowerer may reference; `None` is an explicit promise that no
    /// callable is emitted. The two declaration-keyed collections are maintained as complements so
    /// rerunning registration cannot leave a stale positive/negative answer behind.
    pub(crate) fn record_fn_facade(
        &mut self,
        file: u32,
        declaration: DeclId,
        facade: Option<TypeName>,
    ) {
        let key = (file, declaration.0);
        if let Some(facade) = facade {
            self.register_source_package_for_owner(facade);
            self.unemitted_fn_facades_by_decl.remove(&key);
            self.fn_facades_by_decl.insert(key, facade);
        } else {
            self.fn_facades_by_decl.remove(&key);
            self.unemitted_fn_facades_by_decl.insert(key);
        }
    }

    /// Whether JVM registration explicitly decided that this declaration has no callable facade.
    /// `false` does not imply emission: callers must check the positive map separately because an
    /// analysis-only pipeline records neither outcome.
    #[cfg(test)]
    pub(crate) fn fn_facade_is_explicitly_unemitted(&self, file: u32, declaration: u32) -> bool {
        self.unemitted_fn_facades_by_decl
            .contains(&(file, declaration))
    }

    pub(crate) fn offset_source_files(&mut self, offset: u32) {
        let offset_signature = |signature: &mut Signature| {
            if let Some(file) = &mut signature.source_file {
                *file += offset;
            }
        };
        for signature in self.funs.values_mut().flatten() {
            offset_signature(signature);
        }
        for class in self.classes.values_mut() {
            class.source_file += offset;
        }
        self.source_class_aliases = std::mem::take(&mut self.source_class_aliases)
            .into_iter()
            .map(|((file, alias), internal)| ((file + offset, alias), internal))
            .collect();
        self.resolved_annotations = std::mem::take(&mut self.resolved_annotations)
            .into_iter()
            .map(|((file, lo, hi), internal)| ((file + offset, lo, hi), internal))
            .collect();
        self.conflicting_top_level_key_by_source =
            std::mem::take(&mut self.conflicting_top_level_key_by_source)
                .into_iter()
                .map(|((file, declaration), key)| ((file + offset, declaration), key))
                .collect();
        for candidates in self.conflicting_top_level_candidates.values_mut() {
            for candidate in &mut candidates.public {
                candidate.file += offset;
            }
            candidates.by_file = std::mem::take(&mut candidates.by_file)
                .into_iter()
                .map(|(file, mut local)| {
                    for candidate in &mut local {
                        candidate.file += offset;
                    }
                    (file + offset, local)
                })
                .collect();
        }
        for signature in self
            .ext_funs
            .values_mut()
            .flat_map(HashMap::values_mut)
            .flatten()
        {
            offset_signature(signature);
        }
        for property in self.ext_props.values_mut().flatten() {
            property.source.0 += offset;
        }
        self.source_props = std::mem::take(&mut self.source_props)
            .into_iter()
            .map(|((file, declaration), property)| ((file + offset, declaration), property))
            .collect();
        self.anonymous_object_types = std::mem::take(&mut self.anonymous_object_types)
            .into_iter()
            .map(|((file, declaration), internal)| ((file + offset, declaration), internal))
            .collect();
        self.anonymous_object_captures = std::mem::take(&mut self.anonymous_object_captures)
            .into_iter()
            .map(|((file, declaration), captures)| ((file + offset, declaration), captures))
            .collect();
        self.anonymous_object_capture_discovered =
            std::mem::take(&mut self.anonymous_object_capture_discovered)
                .into_iter()
                .map(|(file, declaration)| (file + offset, declaration))
                .collect();
        self.source_generic_value_operand_slots =
            std::mem::take(&mut self.source_generic_value_operand_slots)
                .into_iter()
                .map(|((file, declaration), slots)| ((file + offset, declaration), slots))
                .collect();
        self.source_projected_return_hazards =
            std::mem::take(&mut self.source_projected_return_hazards)
                .into_iter()
                .map(|(file, declaration)| (file + offset, declaration))
                .collect();
        self.source_ext_funs = std::mem::take(&mut self.source_ext_funs)
            .into_iter()
            .map(|((file, declaration), target)| ((file + offset, declaration), target))
            .collect();
        for class in self.classes.values_mut() {
            for signature in class.methods.values_mut().flatten() {
                offset_signature(signature);
            }
            for signature in class.member_ext_funs.values_mut().flatten() {
                offset_signature(&mut signature.signature);
            }
        }
    }

    pub fn resolved_annotation(&self, file: u32, annotation: &AnnotationRef) -> Option<TypeName> {
        self.resolved_annotations
            .get(&(file, annotation.span.lo, annotation.span.hi))
            .map(|binding| binding.classifier)
    }

    pub(super) fn declaration_suppresses_visibility(
        &self,
        declaration: crate::fir::DeclarationId,
    ) -> bool {
        self.declaration_visibility_suppressions
            .get(&declaration)
            .copied()
            .is_some_and(VisibilitySuppressions::bypasses_visibility)
    }

    pub(super) fn visibility_suppressed_declarations(
        &self,
    ) -> impl Iterator<Item = (crate::fir::DeclarationId, VisibilitySuppressions)> + '_ {
        self.declaration_visibility_suppressions
            .iter()
            .map(|(&declaration, &suppressions)| (declaration, suppressions))
    }

    pub fn annotation_retention(
        &self,
        annotation: TypeName,
    ) -> Option<crate::types::AnnotationRetention> {
        self.annotation_retentions.get(&annotation).copied()
    }

    /// The published `@Target` policies; an annotation with no valid one is applicable everywhere,
    /// which is what Kotlin's use-site defaulting assumes.
    pub fn annotation_targets(&self) -> &crate::types::DeclaredTargetPolicies {
        &self.annotation_targets
    }

    /// Insert under the class's own internal name (the map's key scheme).
    pub fn insert_class(&mut self, sig: ClassSig) -> Option<ClassSig> {
        let internal = sig.internal;
        self.insert_class_sig(internal, sig)
    }

    pub fn insert_class_sig(&mut self, internal: TypeName, sig: ClassSig) -> Option<ClassSig> {
        debug_assert_eq!(internal, sig.internal);
        self.register_source_package_for_owner(internal);
        self.declared_type_parameter_bounds.record_classifier(&sig);
        self.classes.insert(internal, sig)
    }

    /// Record every bound of each collected source callable's type parameters that has several.
    /// Called once callable signatures carry their generic shapes and before any body is inferred.
    pub(crate) fn index_callable_type_parameter_bounds(&mut self) {
        let callables = type_parameter_bounds::DeclaredBoundsIndex::from_callables(self);
        self.declared_type_parameter_bounds.extend(callables);
    }

    /// The declared bounds after the first of source type parameter `identity`.
    pub(crate) fn type_parameter_extra_bounds(&self, identity: &str) -> Vec<Ty> {
        self.declared_type_parameter_bounds.extra(identity)
    }

    /// Every declared bound of source type parameter `identity` that has several.
    pub(crate) fn type_parameter_bounds(&self, identity: &str) -> Vec<Ty> {
        self.declared_type_parameter_bounds.all(identity)
    }

    pub fn ext_prop(&self, recv: Ty, name: &str) -> Option<(Ty, bool)> {
        for key in recv.erased_recv_candidates() {
            let mut applicable = self
                .ext_props
                .get(&(key, name.to_string()))
                .into_iter()
                .flatten()
                .filter(|signature| !recv.is_nullable() || signature.accepts_nullable_receiver);
            let selected = applicable.next();
            if applicable.next().is_some() {
                return None;
            }
            if let Some(signature) = selected {
                return Some((signature.ty, signature.is_var));
            }
        }
        None
    }

    pub fn source_extension_property(&self, source: (u32, u32)) -> Option<&ExtPropSig> {
        self.ext_props
            .values()
            .flatten()
            .find(|signature| signature.source == source)
    }

    pub fn single_fun(&self, name: &str) -> Option<Signature> {
        self.funs.get(name).and_then(|v| match v.as_slice() {
            [sig] => Some(sig.clone()),
            _ => None,
        })
    }

    pub fn source_function_signature(
        &self,
        name: &str,
        file: u32,
        declaration: DeclId,
    ) -> Option<&Signature> {
        self.funs.get(name)?.iter().find(|signature| {
            signature.source_file == Some(file) && signature.source_decl == Some(declaration)
        })
    }

    pub(super) fn fun_ret_by_erased_params(
        &self,
        name: &str,
        params: &[ErasedTypeKey],
    ) -> Option<Ty> {
        let overloads = self.funs.get(name)?;
        overloads
            .iter()
            .find(|sig| overloads.len() == 1 || erased_params_semantic_key(sig) == params)
            .map(|sig| sig.ret)
    }

    pub fn class_by_type_name(&self, internal: TypeName) -> Option<&ClassSig> {
        self.classes.get(&internal)
    }

    /// Resolve one source file's alias spelling to its target module class. The returned class still
    /// comes from the canonical internal-name table; aliases never manufacture duplicate signatures.
    pub fn source_class_alias(&self, file: u32, alias: &str) -> Option<&ClassSig> {
        self.source_class_aliases
            .get(&(file, alias.to_string()))
            .and_then(|internal| self.classes.get(internal))
    }

    /// Select a module class from a file's already-normalized name-resolution candidates.
    ///
    /// Both checker and lowerer have file-specific syntax views, but class-table lookup semantics are
    /// identical: an explicit imported classifier wins, then the same-package identity, then a source
    /// alias edge. Centralizing that identity selection prevents either consumer from growing a
    /// file/module/classpath-specific fallback or accidentally querying [`Self::classes`] by spelling.
    pub fn source_class_binding<I>(
        &self,
        file: u32,
        explicit_candidates: I,
        same_package: TypeName,
        alias: &str,
    ) -> Option<&ClassSig>
    where
        I: IntoIterator<Item = TypeName>,
    {
        self.source_class_identity(file, explicit_candidates, same_package, alias)
            .and_then(|internal| self.classes.get(&internal))
    }

    /// Select the semantic classifier identity even while its complete `ClassSig` is still being
    /// collected. This is the declaration-header twin of [`Self::source_class_binding`]; both use the
    /// same precedence and alias edges, so early inference cannot grow its own import/package branch.
    fn source_class_identity<I>(
        &self,
        file: u32,
        explicit_candidates: I,
        same_package: TypeName,
        alias: &str,
    ) -> Option<TypeName>
    where
        I: IntoIterator<Item = TypeName>,
    {
        let known = |internal: &TypeName| {
            self.source_class_headers.contains_key(internal) || self.classes.contains_key(internal)
        };
        explicit_candidates
            .into_iter()
            .find(|internal| known(internal))
            .or_else(|| known(&same_package).then_some(same_package))
            .or_else(|| {
                self.source_class_aliases
                    .get(&(file, alias.to_string()))
                    .copied()
            })
    }

    /// The normalized classifier record available before full member signatures are collected.
    /// Early inference uses the same direct-supertype graph as every later hierarchy traversal.
    pub(crate) fn source_class_header_shape(
        &self,
        internal: TypeName,
    ) -> Option<crate::libraries::LibraryType> {
        let header = self.source_class_headers.get(&internal)?;
        let mut shape = crate::libraries::LibraryType::declaration_header();
        shape.kind = if header.flags.has(ClassFlags::ANNOTATION) {
            crate::libraries::TypeKind::Annotation
        } else if header.flags.has(ClassFlags::INTERFACE) {
            crate::libraries::TypeKind::Interface
        } else if header.flags.has(ClassFlags::OBJECT) {
            crate::libraries::TypeKind::Object
        } else {
            crate::libraries::TypeKind::Class
        };
        shape.supertypes = header.direct_supertypes.clone();
        shape.supertype_templates = header
            .direct_supertypes
            .iter_ids()
            .map(Ty::obj_name)
            .collect();
        Some(shape)
    }

    /// Find the source declaration that owns a member property across the complete module hierarchy.
    /// Collected signatures flatten inherited property shapes into `ClassSig::props`, but declaration
    /// facts such as visibility and setter visibility remain only in the owner's `declared_props`.
    /// Keep this as the single graph walk for consumers that need declaration facts, so inherited
    /// interface and superclass properties cannot acquire guessed visibility independently.
    pub fn declared_member_prop(
        &self,
        internal: TypeName,
        name: &str,
    ) -> Option<(TypeName, &DeclaredPropertySig)> {
        let mut pending = vec![internal];
        let mut seen = crate::fx_hash::HashSet::default();
        while let Some(current) = pending.pop() {
            if !seen.insert(current) {
                continue;
            }
            let Some(class) = self.class_by_type_name(current) else {
                continue;
            };
            if let Some(declared) = class
                .declared_props
                .get(name)
                .filter(|property| property.source_visible)
            {
                return Some((current, declared));
            }
            // A class member wins over an interface member. Push interfaces in reverse declaration
            // order first and the superclass last because this is a LIFO worklist: the resulting
            // search is current class -> complete superclass hierarchy -> declared interfaces. This
            // is also the order used when source properties are brought into checker scope. Keeping
            // the order here explicit prevents a visibility lookup from selecting an interface
            // declaration while the ordinary property lookup selected a same-named superclass member.
            let interfaces = class.interface_names().collect::<Vec<_>>();
            pending.extend(interfaces.into_iter().rev());
            if let Some(superclass) = class.super_internal_name() {
                pending.push(superclass);
            }
        }
        None
    }
}

/// Shapes that lower correctly ONLY when an `inline` body is spliced into a caller — the checker
/// analyses the body with splice assumptions, so emitting it standalone (a facade static for
/// cross-file calls) would miscompile: a lambda that is STORED or returned rather than passed to
/// a call (capture analysis + synthetic numbering assume the caller's file), anonymous objects,
/// `try` / `break` / `continue` / a LABELED or expression-position `return` (inline-frame control
/// flow — a plain unlabeled value-`return` is just a method return), and `is`/`as` on a type
/// parameter (splicing substitutes the call-site type argument; a standalone body can only erase
/// to the bound). A lambda passed directly to a call is screened by its body because it can be
/// emitted as an ordinary closure; a semantically resolved contract-declaration block is erased,
/// not a closure. Conservative: the file just skips.
/// `true` when every use of a `reified` type parameter inside `f`'s body is a class literal
/// (`T::class`), the sole reified realization the EMITTED-method path models. Explicit call type
/// arguments naming a reified parameter (a nested reified splice would need `is`/`as` markers) and
/// any other expression-position use reject the function.
fn reified_uses_are_class_literals(file: &File, f: &FunDecl) -> bool {
    fn scan(file: &File, e: ExprId, reified: &crate::fx_hash::HashSet<String>) -> bool {
        if let Some(type_args) = file.call_type_args.get(&e.0) {
            if type_args
                .iter()
                .any(|argument| reified.contains(&argument.name))
            {
                return false;
            }
        }
        match file.expr(e) {
            Expr::CallableRef {
                receiver: Some(receiver),
                name,
                ..
            } if name == "class" => {
                // The receiver NAME is the sanctioned use; nothing beneath it to scan.
                if matches!(file.expr(*receiver), Expr::Name(_)) {
                    return true;
                }
                scan(file, *receiver, reified)
            }
            Expr::Name(n) if reified.contains(n) => false,
            // Non-safe `is T`/`as T` emit INSTANCEOF/CHECKCAST markers; `as? T` and a nullable
            // `is T?` target stay splice-only (their branchy realization is not marker-modeled).
            Expr::Is { ty, operand, .. } if reified.contains(&ty.name) => {
                !ty.nullable() && scan(file, *operand, reified)
            }
            Expr::As {
                ty,
                operand,
                nullable,
            } if reified.contains(&ty.name) => {
                !*nullable && !ty.nullable() && scan(file, *operand, reified)
            }
            _ => {
                // `any_child_*` short-circuits on `true`, so "any child is BAD" composes directly.
                !file.any_child_expr(e, &mut |child| !scan(file, child, reified), &mut |stmt| {
                    file.any_child_stmt(stmt, &mut |child| !scan(file, child, reified))
                })
            }
        }
    }
    let reified = &f.reified_type_params;
    match &f.body {
        FunBody::Expr(body) | FunBody::Block(body) => scan(file, *body, reified),
        FunBody::None => false,
    }
}

fn inline_body_has_splice_only_shape(file: &File, f: &FunDecl) -> bool {
    let reified = &f.reified_type_params;
    fn bad_expr(
        file: &File,
        e: ExprId,
        tparams: &[String],
        reified: &crate::fx_hash::HashSet<String>,
    ) -> bool {
        if file.anonymous_object_classes.contains_key(&e) {
            return true;
        }
        match file.expr(e) {
            // A REIFIED fn is emitted as a real method (kotlinc always does), and a standalone
            // `try` lowers there exactly as in any ordinary function — only the splice-time
            // re-lowering concern applies, which the reified path never takes for the emitted
            // body. Non-local returns inside its lambdas stay rejected by the arms below.
            Expr::Try { .. } if !reified.is_empty() => file.any_child_expr(
                e,
                &mut |child| bad_expr(file, child, tparams, reified),
                &mut |stmt| bad_stmt(file, stmt, tparams, reified),
            ),
            Expr::Lambda { .. }
            | Expr::Try { .. }
            | Expr::Break { .. }
            | Expr::Continue { .. }
            | Expr::Return { .. } => true,
            // A REIFIED parameter's is/as emits reification markers (screened separately by
            // `reified_uses_are_class_literals`); a plain erased parameter's cannot stand alone.
            Expr::Is { ty, .. } | Expr::As { ty, .. }
                if tparams.contains(&ty.name) && !reified.contains(&ty.name) =>
            {
                true
            }
            // A lambda in DIRECT call-argument position is safe standalone: it splices with an
            // inline callee or closes over like any fn body. Its BODY is screened (a stored or
            // returned lambda stays rejected above), and a `return` inside it — a non-local
            // return through the inline frame — keeps the fn splice-only.
            Expr::Call { callee, args } => {
                bad_expr(file, *callee, tparams, reified)
                    || args.iter().any(|&arg| match file.expr(arg) {
                        Expr::Lambda { body, .. } => {
                            lambda_body_has_return(file, *body)
                                || bad_expr(file, *body, tparams, reified)
                        }
                        _ => bad_expr(file, arg, tparams, reified),
                    })
            }
            _ => file.any_child_expr(
                e,
                &mut |child| bad_expr(file, child, tparams, reified),
                &mut |stmt| bad_stmt(file, stmt, tparams, reified),
            ),
        }
    }
    fn bad_stmt(
        file: &File,
        s: StmtId,
        tparams: &[String],
        reified: &crate::fx_hash::HashSet<String>,
    ) -> bool {
        if matches!(
            file.stmt(s),
            Stmt::Break(_) | Stmt::Continue(_) | Stmt::Return(_, Some(_))
        ) {
            return true;
        }
        file.any_child_stmt(s, &mut |child| bad_expr(file, child, tparams, reified))
    }
    /// Any `return` inside a lambda argument's body (non-local through the inline frame — its
    /// framing only holds when spliced into a caller, not emitted standalone).
    fn lambda_body_has_return(file: &File, e: ExprId) -> bool {
        if matches!(file.expr(e), Expr::Return { .. }) {
            return true;
        }
        file.any_child_expr(e, &mut |c| lambda_body_has_return(file, c), &mut |s| {
            matches!(file.stmt(s), Stmt::Return(..))
                || file.any_child_stmt(s, &mut |c| lambda_body_has_return(file, c))
        })
    }
    match &f.body {
        FunBody::Expr(body) | FunBody::Block(body) => {
            bad_expr(file, *body, &f.type_params, reified)
        }
        FunBody::None => true,
    }
}

impl SymbolTable {
    /// Whether an `inline fun` may be lowered + emitted as a callable facade static for CROSS-FILE
    /// calls: syntactically (inline and non-reified), and its body carries no splice-only
    /// shape (the checker analyses an `inline` body with splice assumptions). Signature
    /// representation is deliberately absent here: value-class mangling/erasure belongs to the
    /// emission pass, which rewrites the declaration and exact selected call together.
    pub fn inline_fn_facade_emittable(&self, file: &File, f: &FunDecl) -> bool {
        if !f.is_inline() {
            return false;
        }
        // A REIFIED inline fn emits a real erased method (kotlinc's shape) whose reified-parameter
        // uses become `reifiedOperationMarker` placeholders — but only the CLASS-LITERAL use
        // (`T::class`/`T::class.java`) is realized so far. Any other reified use (an `is T`/`as T`
        // marker, or a reified name flowing into a nested reified call's explicit type argument)
        // keeps the fn splice-only.
        if !f.reified_type_params.is_empty() && !reified_uses_are_class_literals(file, f) {
            return false;
        }
        !inline_body_has_splice_only_shape(file, f)
    }

    /// Whether a source function has a standalone callable body in the checked program. Ordinary
    /// functions always do; an inline function does only when its established callable-extension
    /// path or the conservative standalone-body analysis supports it. This semantic answer is
    /// shared by common IR lowering and target registration so neither layer reconstructs the
    /// declaration-kind predicate or introduces file/module/classpath exceptions.
    pub fn source_fn_has_callable_body(&self, file: &File, function: &FunDecl) -> bool {
        !function.is_inline()
            || function.has_callable_inline_extension_body()
            || self.inline_fn_facade_emittable(file, function)
    }

    pub fn source_constructor_matcher(&self) -> SourceConstructorMatcher<'_> {
        SourceConstructorMatcher {
            symbols: self,
            libraries: &*self.libraries,
        }
    }

    pub(super) fn source_constructor_matcher_with<'a>(
        &'a self,
        libraries: &'a dyn SemanticPlatform,
    ) -> SourceConstructorMatcher<'a> {
        SourceConstructorMatcher {
            symbols: self,
            libraries,
        }
    }

    /// The source enum entry names for this classifier identity.
    pub fn enum_entries_of(&self, internal: TypeName) -> Option<&Vec<String>> {
        self.classes
            .get(&internal)
            .filter(|class| class.is_enum())
            .map(|class| &class.enum_entries)
    }

    /// The source spelling (`Wrapper`, `Outer.Inner` for a nested class) of a MODULE class's internal
    /// name. Derived from the internal name itself — the map is keyed by it, so a same-simple-name
    /// class in another package cannot shadow the answer.
    pub fn class_simple_name(&self, internal: TypeName) -> Option<String> {
        self.classes
            .contains_key(&internal)
            .then(|| internal.segment().replace('$', "."))
    }

    /// Source extension overloads applicable to `recv`, most-specific first.
    pub fn ext_fun_overloads(&self, recv: Ty, name: &str) -> ExtensionOverloads<'_, 'static> {
        self.ext_fun_overloads_filtered(recv, name, None, None, None)
    }

    pub fn ext_fun_overloads_in_scope<'symbols, 'scope>(
        &'symbols self,
        recv: Ty,
        name: &str,
        packages: &'scope [TypeName],
    ) -> ExtensionOverloads<'symbols, 'scope> {
        self.ext_fun_overloads_filtered(recv, name, Some(packages), None, None)
    }

    pub(crate) fn ext_fun_overloads_in_import_scope<'symbols, 'scope>(
        &'symbols self,
        recv: Ty,
        name: &'scope str,
        scope: &'scope crate::symbol_resolver::FunctionImportScope,
    ) -> ExtensionOverloads<'symbols, 'scope> {
        self.ext_fun_overloads_filtered(recv, name, None, Some(scope), Some(name))
    }

    fn ext_fun_overloads_filtered<'symbols, 'scope>(
        &'symbols self,
        recv: Ty,
        name: &str,
        packages: Option<&'scope [TypeName]>,
        import_scope: Option<&'scope crate::symbol_resolver::FunctionImportScope>,
        scope_name: Option<&'scope str>,
    ) -> ExtensionOverloads<'symbols, 'scope> {
        let Some(families) = self.ext_funs.get(name) else {
            return ExtensionOverloads {
                exact: None,
                ranked: Vec::new(),
                packages,
                import_scope,
                scope_name,
            };
        };
        let key = recv.extension_recv_key();
        let exact = families.get(&key).map(Vec::as_slice);
        if exact.is_some() && families.len() == 1 {
            return ExtensionOverloads {
                exact,
                ranked: Vec::new(),
                packages,
                import_scope,
                scope_name,
            };
        }
        let mut ranked = Vec::new();
        let module = crate::module_symbols::ModuleSymbols::new(self);
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn SymbolSource,
            &*self.libraries as &dyn SymbolSource,
        ]);
        let mro = crate::symbol_resolver::ReceiverMro::new(&source, recv);
        for (candidate, overloads) in families {
            if *candidate == key {
                continue;
            }
            let Some(rank) = mro.rank(&source, *candidate) else {
                continue;
            };
            ranked.extend(overloads.iter().map(|signature| (rank, signature)));
        }

        ranked.sort_by_key(|(rank, signature)| {
            (
                *rank,
                signature.source_file.unwrap_or(u32::MAX),
                signature
                    .source_decl
                    .map_or(u32::MAX, |declaration| declaration.0),
                signature.params.len(),
            )
        });
        ExtensionOverloads {
            exact,
            ranked,
            packages,
            import_scope,
            scope_name,
        }
    }

    /// Find an extension signature by source declaration identity.
    pub fn source_extension_function(
        &self,
        file: u32,
        declaration: DeclId,
    ) -> Option<(Ty, &Signature)> {
        let (name, receiver, index) = self.source_ext_funs.get(&(file, declaration.0))?;
        let signature = self.ext_funs.get(name)?.get(receiver)?.get(*index)?;
        signature
            .source_receiver
            .map(|declared_receiver| (declared_receiver, signature))
    }

    pub fn source_extension_signature(
        &self,
        name: &str,
        file: u32,
        declaration: DeclId,
    ) -> Option<(Ty, &Signature)> {
        let (declared_name, receiver, index) = self.source_ext_funs.get(&(file, declaration.0))?;
        (declared_name == name)
            .then(|| {
                self.ext_funs
                    .get(declared_name)?
                    .get(receiver)?
                    .get(*index)
                    .map(|signature| (*receiver, signature))
            })
            .flatten()
    }

    pub fn class_by_type_name_mut(&mut self, internal: TypeName) -> Option<&mut ClassSig> {
        self.classes.get_mut(&internal)
    }

    pub fn subclass_names_of(&self, internal: TypeName) -> Vec<TypeName> {
        // Direct subtypes: a subclass names the sealed base in `super_internal` (`class B : S()`),
        // but an implementer of a sealed INTERFACE names it in `interfaces` (`class B : S`) — cover
        // both, so a `when` over a sealed interface is proven exhaustive like a sealed class.
        self.classes
            .values()
            .filter(|c| {
                c.super_internal == Some(internal) || c.interfaces.iter_ids().any(|i| i == internal)
            })
            .map(ClassSig::internal_name)
            .collect()
    }
}
