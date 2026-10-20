//! How a DECLARED type was SPELLED in source, carried alongside the expanded [`Ty`] it resolved to.
//!
//! Kotlin's `@Metadata` records both forms of a declared type: the expanded classifier
//! (`Type.class_name`) and, when the source named a `typealias`, the spelling
//! (`Type.abbreviated_type` = field 13, whose `Type.type_alias_name` = field **12** names the
//! alias). `fun make(c: Cargo): Cargo` with `typealias Cargo = Payload` writes
//! `Type{class_name=Payload, abbreviated_type=Type{type_alias_name=Cargo}}`.
//!
//! This lives BESIDE `Ty` rather than inside it. [`Ty`](crate::types::Ty) is `Copy + PartialEq + Eq
//! + Hash` and interned, and the whole compiler compares types structurally; an alias slot would
//! make `Obj(Payload, alias = Cargo) != Obj(Payload, alias = None)` and silently split every type
//! comparison, interner bucket, and hash lookup on a distinction that is pure surface syntax.
//! kotlinc keeps abbreviation off type equality for the same reason. Alias spelling is metadata
//! input, checked annotations on the mirrored occurrences are semantic plugin input, and the
//! selected written classifier boundary is a finalized frontend-diagnostic fact. The expanded
//! `Ty` remains the single source of type identity and structure.
//!
//! Field numbers here were read off kotlinc 2.4.10 output, not recalled — see
//! `docs/METADATA_NOTES.md`.

use crate::types::{ResolvedAnnotation, Ty, TyKind, TypeName};

/// The source spelling of one declared type, mirroring the expanded `Ty` tree node for node.
///
/// The default means "no source-side fact was retained at or below this node", which is the
/// overwhelmingly common case and allocates nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Spelled {
    /// Zero-based written segment where resolution first selected a classifier for this source
    /// occurrence. This frontend-only binding fact lets Pass 2 diagnose type arguments on package
    /// segments without reopening lookup or inspecting the expanded classifier identity.
    pub classifier_segment: Option<u32>,
    /// This occurrence was written as a definitely-non-null intersection (`T & Any`). The semantic
    /// [`Ty`] already carries that fact in its occurrence bound; this sidecar preserves the exact
    /// Kotlin-metadata flag without making it part of structural type identity.
    pub definitely_non_null: bool,
    /// This occurrence's definitely-non-null intersection was written at the current use site.
    /// `definitely_non_null` also includes a typealias right-hand side inherited by expansion;
    /// reified-call diagnostics distinguish that declaration-site fact from `T & Any` written in
    /// the call's own type argument without reopening the [`TypeRef`](crate::ast::TypeRef).
    pub use_site_definitely_non_null: bool,
    /// The `typealias` named AT this node, fully qualified.
    ///
    /// kotlinc records only the OUTERMOST alias of a chain: with `typealias Cargo = Payload` and
    /// `typealias Chain = Cargo`, a declared `Chain` writes `type_alias_name = Chain`, never a
    /// nested `Cargo`. So this is one level and never itself an alias-to-an-alias reference.
    pub alias: Option<TypeName>,
    /// The alias's AS-SPELLED type arguments, which differ in ARITY from the expanded type's:
    /// `Boxed<Int>` with `typealias Boxed<T> = PBox<T, T>` has ONE entry here and TWO expanded
    /// arguments. These become the abbreviated `Type`'s own `argument` list, so each carries the
    /// `Ty` it resolved to as well as its own spelling. Empty when `alias` is `None`.
    pub alias_args: Vec<(Ty, Spelled)>,
    /// Spellings of the EXPANDED type's arguments, positionally parallel to
    /// [`Ty::type_args`](crate::types::Ty::type_args).
    ///
    /// Populated even when `alias` is `Some`: an alias whose right-hand side itself spells an alias
    /// propagates that spelling into the expansion. `typealias CargoBox = PBox<Cargo, Cargo>` used
    /// as `CargoBox` writes abbreviations on BOTH expanded arguments as well as on the node itself.
    /// A short or empty vec leaves the remaining arguments unabbreviated.
    pub args: Vec<Spelled>,
    /// The annotations `@Metadata` records on THIS occurrence (`Type.annotation`, extension field
    /// 100), in source order: every applied annotation whose retention is not `SOURCE`
    /// (`fun f(x: @Mark Item)`, `Holder<@Bin(3) Item>`), each with its checked argument values
    /// once [`Self::seal`] has run.
    pub annotations: Vec<TypeUseAnnotation>,
    /// At a node that named an alias: the annotations the alias's own right-hand side recorded on
    /// its root, which the EXPANDED type inherits ahead of this occurrence's own [`Self::annotations`]
    /// (`typealias Marked = @Kept Item` makes every `Marked` occurrence an annotated `Item`). The
    /// abbreviation names only what this occurrence wrote.
    pub expansion_annotations: Vec<TypeUseAnnotation>,
    /// The checked occurrence carries Kotlin's semantic `@UnsafeVariance` marker even when that
    /// annotation has `SOURCE` retention and is therefore absent from [`Self::annotations`]. This
    /// is a bind-once semantic sidecar: metadata encoders deliberately ignore it.
    pub(crate) unsafe_variance: bool,
    /// The name this occurrence's function-type parameter was written with (`(count: Int) -> Unit`
    /// names the `Int` occurrence `count`). `@Metadata` records it after the occurrence's own
    /// annotations as `@kotlin.ParameterName(name = "count")`.
    pub parameter_name: Option<Box<str>>,
    /// Provider-normalized upper face of an inferred flexible occurrence. The semantic [`Ty`]
    /// retains the lower face; metadata needs both (`MutableList<T>..List<T>?`, or
    /// `Array<T>..Array<out T>?`) and must not rediscover the upper classifier in a backend.
    pub flexible_upper_bound: Option<Ty>,
    /// This occurrence carries kotlinc's `EnhancedNullability` type attribute. Inferred
    /// declarations retain argument-position marks from the checked Java value that produced them.
    pub enhanced_nullability: bool,
}

/// The spelling of a node that names no alias and records no annotation.
const EMPTY: Spelled = Spelled {
    classifier_segment: None,
    definitely_non_null: false,
    use_site_definitely_non_null: false,
    alias: None,
    alias_args: Vec::new(),
    args: Vec::new(),
    annotations: Vec::new(),
    expansion_annotations: Vec::new(),
    unsafe_variance: false,
    parameter_name: None,
    flexible_upper_bound: None,
    enhanced_nullability: false,
};

impl Spelled {
    /// The "nothing was spelled as an alias" spelling — usable as a `&'static` argument at the many
    /// encode sites whose types cannot carry an alias (synthesized members, builtin classifiers).
    pub const NONE: &'static Spelled = &EMPTY;

    /// Whether this node and everything below it carries no source-side fact. The fast path lets
    /// metadata encoding and finalized frontend checks skip the parallel walk entirely.
    pub fn is_none(&self) -> bool {
        self.classifier_segment.is_none()
            && !self.definitely_non_null
            && self.alias.is_none()
            && self.annotations.is_empty()
            && self.expansion_annotations.is_empty()
            && !self.unsafe_variance
            && self.parameter_name.is_none()
            && self.flexible_upper_bound.is_none()
            && !self.enhanced_nullability
            && self.args.iter().all(Spelled::is_none)
    }

    /// Record provider-owned upper faces for every flexible occurrence in inferred `ty`.
    pub(crate) fn record_flexible_upper_bounds(&mut self, ty: Ty, upper_bound: &dyn Fn(Ty) -> Ty) {
        let ty = match ty.kind() {
            TyKind::PlatformNullable(lower) => {
                self.flexible_upper_bound = Some(upper_bound(lower));
                lower
            }
            TyKind::Nullable(inner)
            | TyKind::DefinitelyNotNull(inner)
            | TyKind::InProjection(inner)
            | TyKind::OutProjection(inner)
            | TyKind::StarProjection(inner) => inner,
            _ => ty,
        };
        match ty.kind() {
            TyKind::Obj(_, arguments) => {
                if self.args.len() < arguments.len() {
                    self.args.resize_with(arguments.len(), Spelled::default);
                }
                for (spelling, argument) in self.args.iter_mut().zip(arguments) {
                    spelling.record_flexible_upper_bounds(*argument, upper_bound);
                }
            }
            TyKind::Fun(signature) if !signature.suspend => {
                let count = signature.params.len() + 1;
                if self.args.len() < count {
                    self.args.resize_with(count, Spelled::default);
                }
                for (spelling, parameter) in
                    self.args.iter_mut().zip(signature.params.iter().copied())
                {
                    spelling.record_flexible_upper_bounds(parameter, upper_bound);
                }
                self.args[signature.params.len()]
                    .record_flexible_upper_bounds(signature.ret, upper_bound);
            }
            // A suspend function's metadata result occupies the continuation argument rather than
            // the ordinary last function-type slot. The publication sidecar does not carry that
            // physical layout, so decline nested flexibility instead of attaching it incorrectly.
            TyKind::Fun(_) => {}
            _ => {}
        }
    }

    /// Remove provider intervals at positions Java enhancement already made rigid.
    pub(crate) fn clear_enhanced_flexible_upper_bounds(
        &mut self,
        enhancement: &crate::libraries::TypeEnhancement,
    ) {
        if enhancement.head() {
            self.flexible_upper_bound = None;
        }
        for (spelling, argument) in self.args.iter_mut().zip(enhancement.arguments()) {
            spelling.clear_enhanced_flexible_upper_bounds(argument);
        }
    }

    /// Mark the positions `enhancement` lays out as `EnhancedNullability` occurrences.
    pub(crate) fn mark_enhanced(&mut self, enhancement: &crate::libraries::TypeEnhancement) {
        if enhancement.head() {
            // Java enhancement fixes this occurrence to its non-null face. A previously recorded
            // unqualified Java interval describes the same provider occurrence before enhancement;
            // retaining both would publish an impossible `@EnhancedNullability` platform type.
            self.enhanced_nullability = true;
            self.flexible_upper_bound = None;
        }
        let arguments = enhancement.arguments();
        if self.args.len() < arguments.len() {
            self.args.resize_with(arguments.len(), Spelled::default);
        }
        for (spelling, argument) in self.args.iter_mut().zip(arguments) {
            spelling.mark_enhanced(argument);
        }
    }

    /// Merge checker-owned publication attributes into an existing declaration spelling.
    ///
    /// Aliases, type-use annotations, parameter names, and definitely-non-null spelling belong to
    /// the written declaration. Flexible upper faces and Java enhancement marks belong to the
    /// checked inferred value. Joining the sidecars occurrence by occurrence preserves both
    /// owners; replacing the declaration spelling wholesale would erase an explicit typealias.
    pub(crate) fn merge_inferred_publication(&mut self, inferred: &Spelled) {
        if let Some(upper) = inferred.flexible_upper_bound {
            self.flexible_upper_bound = Some(upper);
        }
        self.enhanced_nullability |= inferred.enhanced_nullability;
        if self.args.len() < inferred.args.len() {
            self.args.resize_with(inferred.args.len(), Spelled::default);
        }
        for (declared, inferred) in self.args.iter_mut().zip(&inferred.args) {
            declared.merge_inferred_publication(inferred);
        }
    }

    /// This node's spelling for the type argument at `index`, or the empty spelling when the
    /// argument list is short (the common "only some arguments spelled an alias" shape).
    pub fn arg(&self, index: usize) -> &Spelled {
        self.args.get(index).unwrap_or(Spelled::NONE)
    }

    /// This spelling lifted to describe an `Array<Self>`: the array itself spells no alias, its
    /// sole ELEMENT does.
    ///
    /// A `vararg xs: Cargo` parameter is SPELLED as the element but RECORDED as `Array<Cargo>`, so
    /// applying the element's spelling to the array directly would claim the array was the alias.
    pub fn as_array_element(&self) -> Spelled {
        Spelled {
            definitely_non_null: false,
            alias: None,
            alias_args: Vec::new(),
            args: vec![self.clone()],
            ..Spelled::default()
        }
    }

    /// A spelling that names `alias` at this node with no arguments — the plain
    /// `typealias Cargo = Payload` case.
    pub fn of_alias(alias: TypeName) -> Spelled {
        Spelled {
            alias: Some(alias),
            ..Spelled::default()
        }
    }

    pub(crate) fn storage_payload_bytes(&self) -> usize {
        self.alias_args.len() * std::mem::size_of::<(Ty, Spelled)>()
            + self.args.len() * std::mem::size_of::<Spelled>()
            + self
                .annotations
                .iter()
                .chain(&self.expansion_annotations)
                .map(|annotation| {
                    std::mem::size_of::<TypeUseAnnotation>()
                        + match annotation {
                            TypeUseAnnotation::Bound(_) => 0,
                            TypeUseAnnotation::Checked(annotation) => {
                                annotation.arguments.len()
                                    * std::mem::size_of::<(String, crate::types::AnnotationValue)>()
                            }
                        }
                })
                .sum::<usize>()
            + self.parameter_name.as_deref().map_or(0, str::len)
            + self
                .alias_args
                .iter()
                .map(|(_, spelling)| spelling.storage_payload_bytes())
                .sum::<usize>()
            + self
                .args
                .iter()
                .map(Spelled::storage_payload_bytes)
                .sum::<usize>()
    }
}

/// One type-use annotation application on a [`Spelled`] node.
///
/// Pass 1 binds a source application's classifier while declaration spellings are collected, but
/// its argument values are constant expressions the checker can fold only once signatures are
/// final. Such an application therefore enters the tree as [`Self::Bound`] and leaves it, at the
/// stable-metadata boundary, as [`Self::Checked`] (see [`Spelled::seal`]); nothing past that
/// boundary may meet a `Bound` one. An application decoded from a dependency is `Checked` from
/// the start.
#[derive(Clone, Debug, PartialEq)]
pub enum TypeUseAnnotation {
    Bound(AnnotationOccurrence),
    Checked(ResolvedAnnotation),
}

impl TypeUseAnnotation {
    /// The checked application. Panics on a `Bound` one: the stable-metadata boundary seals every
    /// spelling it publishes.
    pub fn checked(&self) -> &ResolvedAnnotation {
        match self {
            Self::Checked(annotation) => annotation,
            Self::Bound(occurrence) => panic!(
                "type-use annotation at {occurrence:?} crossed the stable-metadata boundary unchecked"
            ),
        }
    }
}

/// A source annotation application, by its file and the exact span of its classifier reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AnnotationOccurrence {
    pub source: u32,
    pub span: crate::diag::Span,
}

/// What the checker made of each source type-use application. `metadata` is absent for a
/// `SOURCE`-retained annotation, while semantic facts needed after sealing remain typed here.
pub(crate) struct CheckedTypeUseAnnotation {
    pub(crate) metadata: Option<ResolvedAnnotation>,
    pub(crate) unsafe_variance: bool,
}

pub(crate) type CheckedTypeUseAnnotations =
    crate::fx_hash::HashMap<AnnotationOccurrence, CheckedTypeUseAnnotation>;

impl Spelled {
    /// Replace every bound source application in this tree with its checked value, dropping the
    /// `SOURCE`-retained ones. An application the checker produced no value for is dropped and
    /// pushed onto `unchecked` for the caller to report.
    pub(crate) fn seal(
        &mut self,
        checked: &CheckedTypeUseAnnotations,
        unchecked: &mut Vec<AnnotationOccurrence>,
    ) {
        let mut unsafe_variance = self.unsafe_variance;
        for annotations in [&mut self.annotations, &mut self.expansion_annotations] {
            *annotations = std::mem::take(annotations)
                .into_iter()
                .filter_map(|annotation| match annotation {
                    TypeUseAnnotation::Bound(occurrence) => match checked.get(&occurrence) {
                        Some(value) => {
                            unsafe_variance |= value.unsafe_variance;
                            value.metadata.clone().map(TypeUseAnnotation::Checked)
                        }
                        None => {
                            unchecked.push(occurrence);
                            None
                        }
                    },
                    checked => Some(checked),
                })
                .collect();
        }
        self.unsafe_variance = unsafe_variance;
        for (_, argument) in &mut self.alias_args {
            argument.seal(checked, unchecked);
        }
        for argument in &mut self.args {
            argument.seal(checked, unchecked);
        }
    }
}

impl DeclaredSpellings {
    /// [`Spelled::seal`] over every spelling of the declaration.
    pub(crate) fn seal(
        &mut self,
        checked: &CheckedTypeUseAnnotations,
        unchecked: &mut Vec<AnnotationOccurrence>,
    ) {
        for spelling in [
            &mut self.ret,
            &mut self.getter_ret,
            &mut self.receiver,
            &mut self.superclass,
        ]
        .into_iter()
        .chain(&mut self.params)
        .chain(&mut self.classifier_context_params)
        .chain(&mut self.supertypes)
        .chain(self.type_param_bounds.iter_mut().flatten())
        {
            spelling.seal(checked, unchecked);
        }
    }
}

/// The annotations `@Metadata` records on each type occurrence of one source file, keyed by the
/// occurrence's start offset (the key the parser files type-use annotations under), as Pass 1
/// bound them; see [`Spelled::annotations`].
#[derive(Debug, Default)]
pub(crate) struct RecordedTypeAnnotations {
    annotations: crate::fx_hash::HashMap<u32, Vec<TypeUseAnnotation>>,
    unsafe_variance: crate::fx_hash::HashSet<u32>,
    parameter_names: crate::fx_hash::HashMap<u32, Box<str>>,
}

impl RecordedTypeAnnotations {
    pub(crate) fn record(&mut self, occurrence: u32, annotations: Vec<TypeUseAnnotation>) {
        if !annotations.is_empty() {
            self.annotations.insert(occurrence, annotations);
        }
    }

    /// The occurrence at `occurrence` is the type of a function-type parameter written `name: …`.
    pub(crate) fn record_parameter_name(&mut self, occurrence: u32, name: &str) {
        self.parameter_names.insert(occurrence, name.into());
    }

    pub(crate) fn record_unsafe_variance(&mut self, occurrence: u32) {
        self.unsafe_variance.insert(occurrence);
    }

    /// The annotations recorded on the type occurrence starting at `occurrence`.
    pub(crate) fn at(&self, occurrence: u32) -> &[TypeUseAnnotation] {
        self.annotations
            .get(&occurrence)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub(crate) fn unsafe_variance_at(&self, occurrence: u32) -> bool {
        self.unsafe_variance.contains(&occurrence)
    }

    /// Re-key an already-checked, source-shaped occurrence tree onto one active parser type.
    /// The tree contains no names or semantic types; it is only the structural handoff for
    /// annotation applications whose arguments were folded before temporary source coordinates
    /// were released.
    pub(crate) fn record_checked_source_facts(
        &mut self,
        reference: &crate::ast::TypeRef,
        facts: &Spelled,
    ) -> bool {
        if facts
            .annotations
            .iter()
            .any(|annotation| matches!(annotation, TypeUseAnnotation::Bound(_)))
        {
            return false;
        }
        self.record(reference.span.lo, facts.annotations.clone());
        if facts.unsafe_variance {
            self.record_unsafe_variance(reference.span.lo);
        }
        if let Some(name) = facts.parameter_name.as_deref() {
            self.record_parameter_name(reference.span.lo, name);
        }
        let children = if !reference.fun_params.is_empty() || reference.name == "<fun>" {
            let mut children = reference.fun_params.iter().collect::<Vec<_>>();
            if !reference.fun_suspend() {
                let Some(result) = reference.arg.as_deref() else {
                    return false;
                };
                children.push(result);
            }
            children
        } else {
            reference.targs.iter().collect()
        };
        children.len() == facts.args.len()
            && children
                .into_iter()
                .zip(&facts.args)
                .all(|(child, facts)| self.record_checked_source_facts(child, facts))
    }

    /// The name of the function-type parameter whose type starts at `occurrence`, if it was named.
    pub(crate) fn parameter_name_at(&self, occurrence: u32) -> Option<&str> {
        self.parameter_names.get(&occurrence).map(|name| &**name)
    }
}

/// What one source file says about its type occurrences beyond their resolved types: the alias
/// spellings the parse seam parked when it expanded the file's own aliases away, and the
/// annotations `@Metadata` records on each occurrence.
#[derive(Clone, Copy)]
pub(crate) struct SourceSpellings<'a> {
    pub aliases: &'a crate::fx_hash::HashMap<crate::diag::Span, crate::ast::TypeRef>,
    pub annotations: &'a RecordedTypeAnnotations,
}

/// Source-side facts for ONE declaration's declared types, addressed structurally by semantic role
/// the same way finalized frontend checks and metadata builders address them.
///
/// Every field defaults to "nothing spelled an alias", and a declaration whose whole record would
/// be empty is not recorded at all — so the overwhelming majority of declarations, in the
/// overwhelming majority of modules, cost nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeclaredSpellings {
    /// The declared return type (a property's type, for a property).
    pub ret: Spelled,
    /// A property's explicitly written getter result. It is semantically equal to [`Self::ret`]
    /// when both occur, but each source occurrence can select a different alias spelling.
    pub getter_ret: Spelled,
    /// Parallel to the declaration's LOGICAL value parameters — an extension receiver is recorded
    /// in [`Self::receiver`], not as a leading parameter, matching how `@Metadata` separates them.
    pub params: Vec<Spelled>,
    /// Classifier-owned context parameters, kept separate from primary-constructor parameters.
    /// Callable/property context parameters remain in [`Self::params`], where their stable
    /// signatures place them. A class context parameter is an instance-header role and must not
    /// shift the primary-constructor metadata parameter alignment.
    pub classifier_context_params: Vec<Spelled>,
    /// The extension receiver's spelling (`val Cargo.ext: Cargo` abbreviates both).
    pub receiver: Spelled,
    /// Per declared type parameter, that parameter's declared upper bounds (`<T : Cargo>`).
    pub type_param_bounds: Vec<Vec<Spelled>>,
    /// Class headers only: the declared SUPERCLASS (`class Sub : Super()`), empty when the class
    /// declares none. Kept apart from [`Self::supertypes`] because the emitted supertype list has a
    /// slot for it only when a superclass was declared — and a superclass that declared no alias
    /// spells nothing here, so this field alone cannot say whether that slot exists. Only the
    /// emitter knows.
    pub superclass: Spelled,
    /// Class headers only: the declared INTERFACES, in declaration order.
    pub supertypes: Vec<Spelled>,
}

/// A mandatory semantic header whose compact Pass-1 graph owned only part of its source spelling
/// surface. Selected slots carry their complete Pass-1 spelling. Active slots carry only a
/// source-shaped annotation tree: Pass 1 folds and seals those applications, while active Pass 2
/// performs the type's one and only name binding. Bounds are always selected by type-parameter
/// publication and therefore never deferred here.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DeferredSpellingRole {
    Selected {
        semantic: Option<Ty>,
        spelling: Spelled,
    },
    Active(Spelled),
}

impl DeferredSpellingRole {
    pub(crate) fn as_binding(&self) -> DeclaredSpellingRole<'_> {
        match self {
            Self::Selected { semantic, spelling } => DeclaredSpellingRole::Selected {
                semantic: *semantic,
                spelling,
            },
            Self::Active(facts) => DeclaredSpellingRole::Active(facts),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DeferredDeclaredSpellings {
    pub ret: DeferredSpellingRole,
    pub getter_ret: DeferredSpellingRole,
    pub params: Vec<DeferredSpellingRole>,
    pub receiver: DeferredSpellingRole,
    pub type_param_bounds: Vec<Vec<Spelled>>,
}

impl DeferredDeclaredSpellings {
    pub(crate) fn seal(
        &mut self,
        checked: &CheckedTypeUseAnnotations,
        unchecked: &mut Vec<AnnotationOccurrence>,
    ) {
        for role in [&mut self.ret, &mut self.getter_ret, &mut self.receiver]
            .into_iter()
            .chain(&mut self.params)
        {
            match role {
                DeferredSpellingRole::Selected { spelling, .. }
                | DeferredSpellingRole::Active(spelling) => spelling.seal(checked, unchecked),
            }
        }
        for spelling in self.type_param_bounds.iter_mut().flatten() {
            spelling.seal(checked, unchecked);
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum DeclaredSpellingRole<'a> {
    Selected {
        semantic: Option<Ty>,
        spelling: &'a Spelled,
    },
    Active(&'a Spelled),
}

impl DeclaredSpellingRole<'_> {
    pub(crate) fn into_owned(self) -> DeferredSpellingRole {
        match self {
            Self::Selected { semantic, spelling } => DeferredSpellingRole::Selected {
                semantic,
                spelling: spelling.clone(),
            },
            Self::Active(facts) => DeferredSpellingRole::Active(facts.clone()),
        }
    }
}

/// Read-only spelling roles consumed while an active declaration is checked. Absence of this
/// view is invalid; `None` from one role means only that the explicit deferred record owns it.
#[derive(Clone)]
pub(crate) enum DeclaredSpellingsForBinding {
    Complete(DeclaredSpellings),
    Deferred(DeferredDeclaredSpellings),
}

impl DeclaredSpellingsForBinding {
    pub(crate) fn ret(&self) -> Option<DeclaredSpellingRole<'_>> {
        match self {
            Self::Complete(spellings) => Some(DeclaredSpellingRole::Selected {
                semantic: None,
                spelling: &spellings.ret,
            }),
            Self::Deferred(spellings) => Some(match &spellings.ret {
                role @ DeferredSpellingRole::Selected { .. } => role.as_binding(),
                DeferredSpellingRole::Active(facts) => DeclaredSpellingRole::Active(facts),
            }),
        }
    }

    pub(crate) fn getter_ret(&self) -> Option<DeclaredSpellingRole<'_>> {
        match self {
            Self::Complete(spellings) => Some(DeclaredSpellingRole::Selected {
                semantic: None,
                spelling: &spellings.getter_ret,
            }),
            Self::Deferred(spellings) => Some(match &spellings.getter_ret {
                role @ DeferredSpellingRole::Selected { .. } => role.as_binding(),
                DeferredSpellingRole::Active(facts) => DeclaredSpellingRole::Active(facts),
            }),
        }
    }

    pub(crate) fn receiver(&self) -> Option<DeclaredSpellingRole<'_>> {
        match self {
            Self::Complete(spellings) => Some(DeclaredSpellingRole::Selected {
                semantic: None,
                spelling: &spellings.receiver,
            }),
            Self::Deferred(spellings) => Some(match &spellings.receiver {
                role @ DeferredSpellingRole::Selected { .. } => role.as_binding(),
                DeferredSpellingRole::Active(facts) => DeclaredSpellingRole::Active(facts),
            }),
        }
    }

    pub(crate) fn param(&self, index: usize) -> Option<DeclaredSpellingRole<'_>> {
        match self {
            Self::Complete(spellings) => Some(DeclaredSpellingRole::Selected {
                semantic: None,
                spelling: spellings.param(index),
            }),
            Self::Deferred(spellings) => spellings.params.get(index).map(|role| match role {
                role @ DeferredSpellingRole::Selected { .. } => role.as_binding(),
                DeferredSpellingRole::Active(facts) => DeclaredSpellingRole::Active(facts),
            }),
        }
    }

    pub(crate) fn bound(&self, parameter: usize, bound: usize) -> Option<&Spelled> {
        match self {
            Self::Complete(spellings) => Some(spellings.bound(parameter, bound)),
            Self::Deferred(spellings) => spellings
                .type_param_bounds
                .get(parameter)
                .and_then(|bounds| bounds.get(bound)),
        }
    }
}

impl DeclaredSpellings {
    /// The empty record, for the many builder paths whose declaration spelled no alias.
    pub const NONE: &'static DeclaredSpellings = &DeclaredSpellings {
        ret: EMPTY,
        getter_ret: EMPTY,
        params: Vec::new(),
        classifier_context_params: Vec::new(),
        receiver: EMPTY,
        superclass: EMPTY,
        type_param_bounds: Vec::new(),
        supertypes: Vec::new(),
    };

    /// Whether this declaration retained no source-side fact — the signal not to record it.
    pub fn is_none(&self) -> bool {
        self.ret.is_none()
            && self.getter_ret.is_none()
            && self.receiver.is_none()
            && self.superclass.is_none()
            && self.params.iter().all(Spelled::is_none)
            && self.classifier_context_params.iter().all(Spelled::is_none)
            && self.supertypes.iter().all(Spelled::is_none)
            && self
                .type_param_bounds
                .iter()
                .all(|bounds| bounds.iter().all(Spelled::is_none))
    }

    pub fn param(&self, index: usize) -> &Spelled {
        self.params.get(index).unwrap_or(Spelled::NONE)
    }

    pub fn classifier_context_param(&self, index: usize) -> &Spelled {
        self.classifier_context_params
            .get(index)
            .unwrap_or(Spelled::NONE)
    }

    pub fn supertype(&self, index: usize) -> &Spelled {
        self.supertypes.get(index).unwrap_or(Spelled::NONE)
    }

    /// The spellings for an emitted supertype list, aligned to it.
    ///
    /// `has_declared_superclass` says whether that list leads with a superclass, which it does
    /// exactly when the class declared one — a superclass position is never materialized for an
    /// undeclared `kotlin/Any`. It cannot be inferred from [`Self::superclass`]: a declared
    /// superclass that named no `typealias` spells nothing, yet still occupies the slot. Getting
    /// this wrong shifts every abbreviation onto the neighbouring supertype.
    pub fn supertype_spellings(&self, has_declared_superclass: bool) -> Vec<Spelled> {
        let superclass = has_declared_superclass.then(|| self.superclass.clone());
        superclass
            .into_iter()
            .chain(self.supertypes.iter().cloned())
            .collect()
    }

    pub fn bound(&self, parameter: usize, index: usize) -> &Spelled {
        self.type_param_bounds
            .get(parameter)
            .and_then(|bounds| bounds.get(index))
            .unwrap_or(Spelled::NONE)
    }

    pub(crate) fn storage_payload_bytes(&self) -> usize {
        self.ret.storage_payload_bytes()
            + self.getter_ret.storage_payload_bytes()
            + self.receiver.storage_payload_bytes()
            + self.superclass.storage_payload_bytes()
            + self
                .params
                .iter()
                .map(Spelled::storage_payload_bytes)
                .sum::<usize>()
            + self
                .classifier_context_params
                .iter()
                .map(Spelled::storage_payload_bytes)
                .sum::<usize>()
            + self
                .type_param_bounds
                .iter()
                .flatten()
                .map(Spelled::storage_payload_bytes)
                .sum::<usize>()
            + self
                .supertypes
                .iter()
                .map(Spelled::storage_payload_bytes)
                .sum::<usize>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_spelling_is_none_and_yields_none_for_every_argument() {
        let spelled = Spelled::default();
        assert!(spelled.is_none());
        assert!(spelled.arg(0).is_none());
        assert!(spelled.arg(7).is_none());
    }

    #[test]
    fn an_undeclared_superclass_takes_no_slot_generic_or_not() {
        // `class Holder<T> : Marker` and `class Holder : Marker` emit the SAME supertype list — the
        // interface alone. A reserved slot for the undeclared `kotlin/Any` would land the
        // interface's abbreviation one position early.
        let marker = Spelled::of_alias(crate::types::type_name("app/Marker"));
        let header = DeclaredSpellings {
            superclass: Spelled::default(),
            supertypes: vec![marker.clone()],
            ..DeclaredSpellings::default()
        };
        assert_eq!(header.supertype_spellings(false), vec![marker]);
    }

    #[test]
    fn a_declared_superclass_leads_even_when_it_spells_no_alias() {
        // `class Sub : Base(), Marker` with a plain `Base`: the superclass spells nothing, but it
        // still OCCUPIES the leading position, so `Marker`'s abbreviation must not slide onto it.
        let marker = Spelled::of_alias(crate::types::type_name("app/Marker"));
        let header = DeclaredSpellings {
            superclass: Spelled::default(),
            supertypes: vec![marker.clone()],
            ..DeclaredSpellings::default()
        };
        let aligned = header.supertype_spellings(true);
        assert_eq!(aligned.len(), 2, "the declared superclass holds a slot");
        assert!(aligned[0].is_none(), "a plain superclass spells nothing");
        assert_eq!(aligned[1], marker, "the interface keeps its own spelling");
    }

    #[test]
    fn a_declared_superclass_that_spells_an_alias_leads() {
        let base = Spelled::of_alias(crate::types::type_name("app/Super"));
        let header = DeclaredSpellings {
            superclass: base.clone(),
            supertypes: Vec::new(),
            ..DeclaredSpellings::default()
        };
        assert_eq!(header.supertype_spellings(true), vec![base]);
    }

    #[test]
    fn a_vararg_element_spelling_lifts_under_the_array() {
        // `vararg xs: Cargo` is spelled as the ELEMENT but recorded as `Array<Cargo>`, so the
        // spelling has to move under the array — applying it directly would claim the ARRAY was
        // written as the alias.
        let element = Spelled::of_alias(crate::types::type_name("app/Cargo"));
        let array = element.as_array_element();
        assert_eq!(array.alias, None, "the array itself spells no alias");
        assert_eq!(array.arg(0), &element, "the element keeps the spelling");
        assert!(!array.is_none(), "an alias below the root still counts");
    }

    #[test]
    fn an_alias_below_the_root_makes_the_tree_not_none() {
        // `List<Cargo>`: the root spells `List` directly, the ARGUMENT spells the alias. kotlinc
        // puts `abbreviated_type` on the argument's `Type`, so the root must not report "none".
        let spelled = Spelled {
            args: vec![Spelled::of_alias(crate::types::type_name("dep/Cargo"))],
            ..Spelled::default()
        };
        assert!(!spelled.is_none());
        assert_eq!(
            spelled.arg(0).alias,
            Some(crate::types::type_name("dep/Cargo"))
        );
        assert!(spelled.arg(1).is_none());
    }

    #[test]
    fn suspend_function_flexibility_is_not_written_to_an_ordinary_result_slot() {
        let ty = Ty::fun_with_shape(
            vec![Ty::String],
            Ty::platform_nullable(Ty::String),
            0,
            false,
            true,
        );
        let mut spelled = Spelled::default();
        spelled.record_flexible_upper_bounds(ty, &|lower| lower);

        assert!(spelled.is_none());
    }

    #[test]
    fn inferred_publication_merges_without_erasing_declared_aliases() {
        let alias = crate::types::type_name("sample/Strings");
        let nested_alias = crate::types::type_name("sample/Element");
        let mut declared = Spelled {
            alias: Some(alias),
            args: vec![Spelled::of_alias(nested_alias)],
            ..Spelled::default()
        };
        let inferred = Spelled {
            flexible_upper_bound: Some(Ty::obj("kotlin/collections/List")),
            args: vec![Spelled {
                enhanced_nullability: true,
                ..Spelled::default()
            }],
            ..Spelled::default()
        };

        declared.merge_inferred_publication(&inferred);

        assert_eq!(declared.alias, Some(alias));
        assert_eq!(declared.flexible_upper_bound, inferred.flexible_upper_bound);
        assert_eq!(declared.arg(0).alias, Some(nested_alias));
        assert!(declared.arg(0).enhanced_nullability);
    }

    #[test]
    fn sealing_keeps_source_retained_unsafe_variance_as_a_semantic_fact() {
        let occurrence = AnnotationOccurrence {
            source: 3,
            span: crate::diag::Span::new(11, 26),
        };
        let mut spelled = Spelled {
            annotations: vec![TypeUseAnnotation::Bound(occurrence)],
            ..Spelled::default()
        };
        let checked = CheckedTypeUseAnnotations::from_iter([(
            occurrence,
            CheckedTypeUseAnnotation {
                metadata: None,
                unsafe_variance: true,
            },
        )]);
        let mut unchecked = Vec::new();

        spelled.seal(&checked, &mut unchecked);

        assert!(spelled.annotations.is_empty());
        assert!(spelled.unsafe_variance);
        assert!(unchecked.is_empty());
        assert!(!spelled.is_none());
    }
}
