//! Production lowering and emission of exact selected KLIB dependency bodies.
//!
//! The semantic provider supplies one exact module-qualified body graph and its authenticated
//! intrinsic declarations. Common lowering follows serialized call identities through that graph.
//! A successfully closed unit is emitted before its source caller and publishes every transitive
//! declaration it realizes through the ordinary dependency-export registry, so target call
//! emission performs no lookup or spelling dispatch.

use crate::backend::{
    Artifact, Backend, BackendModuleFacts, CheckedBackendCallables, CheckedBackendClassifiers,
    CheckedIrFile, CheckedIrUnitKind, DependencyExportSignatures,
};
use crate::diag::{DiagSink, Span};
use crate::fx_hash::{HashMap, HashSet};
use crate::ir::{Callee, IrExpr};
use crate::klib_lowering::{DependencyBodyUnit, EmittedDependencyCallable, KlibCalleeFacts};
use crate::libraries::KlibDependencyCallable;
use crate::resolve::PassTwoSymbols;
use crate::types::{Ty, TyKind, TypeName};
use std::collections::VecDeque;

mod accessor_numbering;
mod class_declarations;
mod emitted_roots;

/// Exact dependency support declarations already emitted into one target program.
///
/// This state outlives independently analyzed source modules. It retains the semantic singleton
/// receiver contract selected when each declaration was emitted, but deliberately stores no opaque
/// [`ExternalCallableId`](crate::fir::ExternalCallableId): each module owns a fresh provider and
/// rebinds these exact KLIB identities to that provider's IDs before lowering an external edge.
#[derive(Default)]
pub(super) struct DependencySupportState {
    snapshot: Option<crate::libraries::DependencyBodySnapshot>,
    emitted: HashMap<KlibDependencyCallable, PublishedDependency>,
    /// Exact body declarations owned by the support unit currently being prepared. Recursive
    /// closure may link to these only after that unit has proved it owns the corresponding body.
    in_progress: HashMap<KlibDependencyCallable, PublishedDependency>,
    /// The ordinal of the next support unit. A unit's dependencies close before it is emitted,
    /// so the emitted count does not number units uniquely.
    next_support_unit: usize,
    /// One physical realization per exact logical class; equal names cannot alias producers.
    materialized_classes: HashMap<TypeName, KlibDependencyCallable>,
    /// One static per top-level property backing field, by its library and field symbol. A
    /// unit reserves its statics with its exports, before its dependencies close.
    materialized_statics: HashSet<(String, crate::metadata::klib_ir::KlibIrSymbol)>,
    /// Exact class owner reservations for units whose layout is being prepared but is not emitted
    /// yet. These prevent a recursive dependency from materializing the same class twice.
    materializing_classes: HashMap<TypeName, KlibDependencyCallable>,
}

#[derive(Clone)]
enum PublishedDependency {
    Body(Option<PublishedSingletonDispatch>),
    AllocatingConstructor,
    ConstructorInitializer(crate::libraries::KlibClassIdentity),
}

#[derive(Clone, Copy)]
struct PublishedSingletonDispatch {
    ty: Ty,
    classifier: TypeName,
}

impl DependencySupportState {
    pub(super) fn record_initializer(
        &mut self,
        declaration: &crate::backend::ConstructorInitializerDeclaration,
    ) {
        self.emitted
            .entry(declaration.constructor.clone())
            .or_insert_with(|| {
                PublishedDependency::ConstructorInitializer(declaration.class.clone())
            });
    }

    fn accepts_snapshot(&mut self, snapshot: &crate::libraries::DependencyBodySnapshot) -> bool {
        match &self.snapshot {
            Some(selected) => selected == snapshot,
            None => {
                self.snapshot = Some(snapshot.clone());
                true
            }
        }
    }
}

/// One checked file whose emitted uses dependency support closes over, with its module's facts.
#[derive(Clone, Copy)]
pub(super) struct DependencyUse<'s, 'a> {
    pub(super) file: &'s CheckedIrFile<'a>,
    pub(super) facts: &'s BackendModuleFacts,
}

/// Emit authenticated intrinsic support and concrete ordinary bodies selected by the emitted uses
/// of `uses`, every checked file of the target program at once. Each category closes and
/// validates independently, once across the complete target program. The support units belong to
/// the last use's module, the one every earlier module of the program precedes.
pub(super) fn emit_dependencies<B: Backend>(
    uses: &[DependencyUse<'_, '_>],
    symbols: &PassTwoSymbols,
    backend: &B,
    state: &mut B::State,
    support_state: &mut DependencySupportState,
    diags: &mut DiagSink,
) -> Vec<Artifact> {
    if !backend.emits_dependency_support() {
        return Vec::new();
    }
    let Some(&DependencyUse {
        file: source,
        facts: module_facts,
    }) = uses.last()
    else {
        return Vec::new();
    };
    let platform = symbols.semantic_platform();
    let Some(facts) = platform.klib_dependency_body_facts() else {
        return Vec::new();
    };
    let Some(snapshot) = facts.snapshot() else {
        return Vec::new();
    };
    if !support_state.accepts_snapshot(snapshot) {
        diags.error(
            Span::new(0, 0),
            "internal error: one target emission session cannot combine different KLIB provider snapshots",
        );
        return Vec::new();
    }
    let bodies = facts.bodies();
    let intrinsics = facts.intrinsics();
    let selected = uses
        .iter()
        .flat_map(|use_| use_.file.callables.klib_body_callables())
        .collect::<Vec<_>>();
    let accessors = match accessor_numbering::program_accessors(platform, &selected) {
        Ok(accessors) => accessors,
        Err(error) => {
            diags.error(Span::new(0, 0), format!("internal error: {error}"));
            return Vec::new();
        }
    };
    let selected = match selected
        .into_iter()
        .map(|callable| accessor_numbering::program_numbered(callable, &accessors))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(selected) => selected,
        Err(error) => {
            diags.error(Span::new(0, 0), format!("internal error: {error}"));
            return Vec::new();
        }
    };
    if uses
        .iter()
        .any(|use_| use_.file.callables.klib_type_hierarchy().is_none())
    {
        diags.error(
            Span::new(0, 0),
            "internal error: checked KLIB bodies have no frozen class-hierarchy facts",
        );
        return Vec::new();
    }
    let mut emitted_dependency_callables = HashSet::default();
    for use_ in uses {
        match emitted_roots::emitted_dependency_callables(use_.file) {
            Ok(callables) => emitted_dependency_callables.extend(callables),
            Err(error) => {
                diags.error(Span::new(0, 0), format!("internal error: {error}"));
                return Vec::new();
            }
        }
    }
    let target_represents_classifier =
        |classifier| backend.represents_dependency_classifier(classifier);
    let dispatched_classes = backend.dispatched_dependency_classes();
    // One file's checked graph is its own; several files' graphs are checked again as one, so the
    // class hierarchy answers for every class any of them reaches.
    let program_facts;
    let checked_facts = match uses {
        [only] => only.file.callables.klib_dependency_facts(),
        _ => {
            program_facts = match crate::resolve::for_checked_klib_bodies(
                platform,
                bodies,
                selected.iter().copied(),
                &dispatched_classes,
            ) {
                Ok(facts) => facts,
                Err(identity) => {
                    diags.error(
                        Span::new(0, 0),
                        format!(
                            "internal error: checked KLIB body graph has no exact member facts for {identity:?}"
                        ),
                    );
                    return Vec::new();
                }
            };
            &program_facts
        }
    };
    // The checker froze the selected `enumEntries` as the edge of every synthetic `entries` body;
    // a unit emits that call only for a target whose enums build `entries` by it.
    let entries_factory = backend
        .common_ir_preparation()
        .filter(|preparation| preparation.enums.builds_entries_by_factory())
        .and_then(|_| checked_facts.enum_entries_factory().cloned());
    let callees = KlibCalleeFacts::new(selected.iter().copied())
        .with_checked_dependency_facts(checked_facts)
        .with_enum_entries_factory(entries_factory.clone())
        .with_target_classifiers(&target_represents_classifier);
    let mut roots: Vec<crate::libraries::KlibBodyCallable<'_>> = Vec::new();
    for callable in selected.iter().copied().filter(|callable| {
        emitted_dependency_callables.contains(callable.identity())
            && !support_state.emitted.contains_key(callable.identity())
            && !support_state.in_progress.contains_key(callable.identity())
            && platform.is_platform_stdlib_dependency(callable.identity())
            && DependencyBodyUnit::is_scalar_root_candidate(*callable, bodies)
            && bodies.is_supported_intrinsic_root(callable.identity(), intrinsics)
    }) {
        if !roots
            .iter()
            .any(|root| root.identity() == callable.identity())
        {
            roots.push(callable);
        }
    }
    let target_realizes_intrinsic = |operation| backend.realizes_dependency_intrinsic(operation);
    let target_realizes_getter = |signature: crate::backend::DependencyGetterSignature<'_>| {
        backend.realizes_dependency_getter(signature)
    };
    let capabilities = emitted_roots::OrdinaryRootCapabilities {
        realizes_intrinsic: &target_realizes_intrinsic,
        realizes_getter: &target_realizes_getter,
        represents_classifier: &target_represents_classifier,
        retains_initializer: &|selected| match support_state.emitted.get(selected.identity()) {
            Some(PublishedDependency::AllocatingConstructor) => Ok(true),
            Some(PublishedDependency::ConstructorInitializer(identity)) => {
                if selected.class_contract().is_some_and(|class| {
                    class.module == identity.module
                        && class.signature == identity.signature
                        && class.module == selected.identity().module
                        && selected.declaration_owner()
                            == Some(crate::types::SemanticCallableOwner::Classifier(
                                class.classifier,
                            ))
                }) {
                    Ok(true)
                } else {
                    Err("a retained superclass initializer changed its canonical class contract")
                }
            }
            _ => Ok(false),
        },
    };
    let mut ordinary: Vec<crate::libraries::KlibBodyCallable<'_>> = Vec::new();
    for use_ in uses {
        match emitted_roots::ordinary_roots(
            platform,
            use_.file,
            bodies,
            intrinsics,
            &callees,
            &capabilities,
        ) {
            Ok(roots) => {
                for root in roots {
                    let root = match accessor_numbering::program_numbered(root, &accessors) {
                        Ok(root) => root,
                        Err(error) => {
                            diags.error(Span::new(0, 0), format!("internal error: {error}"));
                            return Vec::new();
                        }
                    };
                    if !ordinary
                        .iter()
                        .any(|known| known.identity() == root.identity())
                    {
                        ordinary.push(root);
                    }
                }
            }
            Err(error) => {
                diags.error(Span::new(0, 0), format!("internal error: {error}"));
                return Vec::new();
            }
        }
    }
    // Category ownership is a declaration capability, not an emission-success test. An intrinsic
    // wrapper whose unsupported body transaction declines must not be retried as an ordinary body.
    let intrinsic_roots: HashSet<_> = roots.iter().map(|root| root.identity()).collect();
    ordinary.retain(|root| {
        let intrinsic_owned = intrinsic_roots.contains(root.identity());
        if intrinsic_owned {
            crate::trace_compiler!(
                "lower",
                "intrinsic support owns dependency root: {:?}",
                root.identity()
            );
        }
        !intrinsic_owned
            && !support_state.emitted.contains_key(root.identity())
            && !support_state.in_progress.contains_key(root.identity())
    });
    let mut root_objects: Vec<crate::libraries::KlibBodyClassContract> = Vec::new();
    for use_ in uses {
        match emitted_roots::ordinary_root_objects(
            platform,
            use_.file,
            bodies,
            &callees,
            &target_represents_classifier,
        ) {
            Ok(objects) => {
                for object in objects {
                    if !root_objects
                        .iter()
                        .any(|known| known.classifier == object.classifier)
                    {
                        root_objects.push(object);
                    }
                }
            }
            Err(error) => {
                diags.error(Span::new(0, 0), format!("internal error: {error}"));
                return Vec::new();
            }
        }
    }
    let root_objects = root_objects
        .into_iter()
        .filter(|object| {
            !support_state
                .materialized_classes
                .contains_key(&object.classifier)
                && !support_state
                    .materializing_classes
                    .contains_key(&object.classifier)
        })
        .collect::<Vec<_>>();
    let mut artifacts = Vec::new();
    for (requires_intrinsic_link, mut roots) in [(true, roots), (false, ordinary)] {
        // Intrinsic support may have emitted an ordinary root transitively in the preceding unit.
        roots.retain(|root| {
            !support_state.emitted.contains_key(root.identity())
                && !support_state.in_progress.contains_key(root.identity())
        });
        let root_objects: &[_] = if requires_intrinsic_link {
            &[]
        } else {
            &root_objects
        };
        if roots.is_empty() && root_objects.is_empty() {
            continue;
        }
        let Some(mut externals) = rebound_externals(
            platform,
            &support_state.emitted,
            &support_state.in_progress,
            diags,
        ) else {
            return Vec::new();
        };
        // An enum or object an earlier unit materialized, or one an enclosing unit is
        // materializing, keeps its one set of constants, or its one instance, there; this unit
        // reads them through that unit's exports.
        let mut unit = DependencyBodyUnit::reading_realized_classes(
            support_state
                .materialized_classes
                .keys()
                .chain(support_state.materializing_classes.keys())
                .copied(),
        )
        .presetting_constant_statics(backend.presets_constant_top_level_initializers())
        .adding_members_to_realized_classes(backend.adds_members_to_realized_classes());
        let capabilities = DependencyLeafCapabilities {
            unit: &unit,
            callees: &callees,
            target_realizes_bodyless: backend.realizes_bodyless_dependency_support(),
            target_realizes_intrinsic: &target_realizes_intrinsic,
            target_realizes_getter: &target_realizes_getter,
            target_represents_classifier: &target_represents_classifier,
        };
        let closure = match emitted_roots::declarations(
            platform,
            bodies,
            intrinsics,
            emitted_roots::ClosureRoots {
                callables: &roots,
                objects: root_objects,
            },
            &externals,
            &capabilities,
            !requires_intrinsic_link,
        ) {
            Ok(closure) => closure,
            Err(error) => {
                diags.error(
                    Span::new(0, 0),
                    format!("cannot close a selected ordinary KLIB body: {error}"),
                );
                return Vec::new();
            }
        };
        let emitted_roots::FrozenClosure {
            declarations,
            authorized_member_edges,
            read_classes,
            dispatched,
            private_member_classes,
        } = closure;
        let callees = KlibCalleeFacts::with_reference_declarations(
            selected.iter().copied(),
            declarations.iter().filter_map(|(target, declaration)| {
                declaration
                    .klib_body_callable()
                    .map(|callable| (*target, callable))
            }),
        )
        .with_checked_dependency_facts(checked_facts)
        .with_authorized_member_edges(authorized_member_edges)
        .with_read_classes(read_classes)
        .with_private_member_classes(private_member_classes)
        .with_enum_entries_factory(entries_factory.clone())
        .with_target_classifiers(&target_represents_classifier);
        let capabilities = DependencyLeafCapabilities {
            unit: &unit,
            callees: &callees,
            target_realizes_bodyless: backend.realizes_bodyless_dependency_support(),
            target_realizes_intrinsic: &target_realizes_intrinsic,
            target_realizes_getter: &target_realizes_getter,
            target_represents_classifier: &target_represents_classifier,
        };
        if !prepare_bodyless_externals(
            platform,
            bodies,
            intrinsics,
            // A member that dispatch reaches is emitted with its class although no walked body
            // calls it, so its body's leaves are prepared as a root's are.
            roots
                .iter()
                .map(|root| root.identity().clone())
                .chain(dispatched),
            &capabilities,
            &mut externals,
            diags,
        ) {
            return Vec::new();
        }
        for callable in roots {
            if let Err(decline) = unit.lower_root_with_declarations(
                callable, bodies, &callees, intrinsics, &externals, platform,
            ) {
                if requires_intrinsic_link && !decline.is_internal_inconsistency() {
                    // A wrapper whose body uses a form lowering does not model yet is not support: the
                    // declined body left the unit as it was, and the backend declines its call sites.
                    crate::trace_compiler!("lower", "intrinsic support root declined: {decline}");
                    continue;
                }
                diags.error(
                    Span::new(0, 0),
                    format!("cannot lower a selected KLIB dependency body: {decline}"),
                );
                return Vec::new();
            }
        }
        for object in root_objects {
            let class = callees
                .read_class(object.classifier)
                .expect("the closure froze the objects the source reads");
            if let Err(decline) =
                unit.lower_object(class, bodies, &callees, intrinsics, &externals, platform)
            {
                diags.error(
                    Span::new(0, 0),
                    format!("cannot lower a library object the source reads: {decline}"),
                );
                return Vec::new();
            }
        }
        if unit.callable_functions().next().is_none()
            && unit.initializing_constructors().next().is_none()
        {
            continue;
        }
        if requires_intrinsic_link && !links_emitted_support(unit.ir(), &externals) {
            diags.error(
            Span::new(0, 0),
            "internal error: authenticated KLIB intrinsic roots lowered without an exact intrinsic or emitted-support edge",
        );
            return Vec::new();
        }
        if let Err(error) = unit.ir().validate_determined_types() {
            diags.error(
                Span::new(0, 0),
                format!("internal error: dependency bodies have an undetermined type: {error:?}"),
            );
            return Vec::new();
        }
        if let Err(error) = unit.ir().validate_semantic_contracts() {
            diags.error(
                Span::new(0, 0),
                format!("internal error: dependency bodies violate common IR contracts: {error:?}"),
            );
            return Vec::new();
        }

        let materialized = unit
            .materialized_classes()
            .map(|(classifier, identity)| (classifier, identity.clone()))
            .collect::<Vec<_>>();
        let value_classes = materialized
            .iter()
            .filter(|(classifier, _)| {
                unit.ir()
                    .class_id_by_name(*classifier)
                    .is_some_and(|class| unit.ir().classes[class as usize].is_value)
            })
            .map(|(classifier, _)| *classifier)
            .collect::<crate::fx_hash::HashSet<_>>();
        for (classifier, identity) in &materialized {
            // Current-module representation facts take precedence in the checked backend view.
            // They cannot describe an unrelated imported class with the same unqualified key.
            if uses.iter().any(|use_| {
                use_.facts.classifier(*classifier).is_some()
                    || use_.facts.is_body_local(*classifier)
                    || use_.file.ir.class_id_by_name(*classifier).is_some()
            }) {
                diags.error(
                    Span::new(0, 0),
                    "cannot materialize a dependency class that collides with a current-module classifier",
                );
                return Vec::new();
            }
            if let Some(previous) = support_state
                .materialized_classes
                .get(classifier)
                .or_else(|| support_state.materializing_classes.get(classifier))
            {
                if previous == identity && value_classes.contains(classifier) {
                    // A value class's members spread over units; each one re-reads the class an
                    // earlier unit published and emits only its own members.
                    continue;
                }
                let message = if previous == identity {
                    "cannot materialize an imported class twice in separate dependency support units"
                } else {
                    "cannot materialize equal classifier names from different KLIB producers in one target program"
                };
                diags.error(Span::new(0, 0), message);
                return Vec::new();
            }
        }
        let statics = unit
            .materialized_statics()
            .map(|(module, field)| (module.to_owned(), field.clone()))
            .collect::<Vec<_>>();
        if statics
            .iter()
            .any(|field| support_state.materialized_statics.contains(field))
        {
            diags.error(
                Span::new(0, 0),
                "cannot store a dependency property twice in separate dependency support units",
            );
            return Vec::new();
        }
        let mut exports = DependencyExportSignatures::default();
        if let Err(error) = class_declarations::record(&unit, platform, &callees, &mut exports) {
            diags.error(
                Span::new(0, 0),
                format!("internal error: cannot publish materialized class declarations: {error}"),
            );
            return Vec::new();
        }

        let closed = unit
            .callable_functions()
            // A reachable bodyless declaration remains an exact provider edge which the target may
            // realize from its frozen callable fact. It is not emitted code and therefore cannot be
            // published as a dependency export: an export is a trampoline to a body in this unit.
            .filter(|(_, function)| {
                crate::backend::has_standalone_dependency_code(unit.ir(), *function)
            })
            .map(|(identity, function)| (identity.clone(), function))
            .collect::<Vec<_>>();
        for (identity, function) in &closed {
            exports.record_callable(*function, identity.clone());
            if let Some(receiver) = unit.member_receiver(identity) {
                exports.record_member_receiver(*function, receiver);
            }
            if let Some(classifier) = unit.authenticated_singleton_classifier(identity) {
                let inert_platform_singleton =
                    bodies.singleton_initialization_is_inert(&identity.module, classifier);
                exports.record_singleton_member_receiver(
                    *function,
                    classifier,
                    !inert_platform_singleton,
                );
            }
            if let Some(&receiver) = unit.ir().singleton_extension_unread_receivers.get(function) {
                let inert_platform_singleton = match receiver.kind() {
                    TyKind::Obj(classifier, arguments) => {
                        arguments.is_empty()
                            && bodies
                                .singleton_initialization_is_inert(&identity.module, classifier)
                    }
                    _ => false,
                };
                exports.record_singleton_extension_unread_receiver(
                    *function,
                    receiver,
                    !inert_platform_singleton,
                );
            }
        }
        let constructors = unit
            .allocating_constructors()
            .map(|(identity, constructor)| (identity.clone(), constructor))
            .collect::<Vec<_>>();
        for (identity, constructor) in &constructors {
            if unit
                .ir()
                .declared_class_self_type(unit.ir().classes[constructor.class as usize].fq_name)
                .is_none()
            {
                diags.error(
                    Span::new(0, 0),
                    "internal error: an allocating constructor lost its checked class self type",
                );
                return Vec::new();
            }
            exports.record_constructor(
                crate::backend::ExportedConstructor {
                    class: constructor.class,
                    secondary: constructor.secondary,
                },
                identity.clone(),
            );
        }
        let mut published = Vec::with_capacity(closed.len());
        for (identity, _) in &closed {
            let Some((_external, realization)) = platform.external_callable_by_dependency(identity)
            else {
                diags.error(
                Span::new(0, 0),
                "internal error: a lowered KLIB dependency body has no exact provider realization",
            );
                return Vec::new();
            };
            if realization.dependency_callable.as_ref() != Some(identity) {
                diags.error(
                    Span::new(0, 0),
                    "internal error: a KLIB dependency body realization changed its exact identity",
                );
                return Vec::new();
            }
            // The provider's stored realization is the undecorated declaration. Retain the singleton
            // receiver of the exact checked edge variant which authenticated the serialized header;
            // input order cannot decide the receiver contract published to later support units.
            let singleton_dispatch = match (
                unit.authenticated_singleton_dispatch(identity),
                unit.authenticated_singleton_classifier(identity),
            ) {
                (Some(ty), Some(classifier)) => Some(PublishedSingletonDispatch { ty, classifier }),
                (None, None) => None,
                _ => {
                    diags.error(
                    Span::new(0, 0),
                    "internal error: an authenticated singleton KLIB body lost its exact classifier",
                );
                    return Vec::new();
                }
            };
            // A value class's box construction is a function of the unit exported under its
            // primary constructor's identity: later units allocate a box through it, as they
            // allocate an ordinary class through its constructor's export.
            let role = if realization.kind == crate::libraries::ExternalCallableKind::Constructor {
                if singleton_dispatch.is_some()
                    || !realization
                        .klib_body_class_contract
                        .as_ref()
                        .is_some_and(|contract| contract.is_value)
                {
                    diags.error(
                        Span::new(0, 0),
                        "internal error: a constructor emitted as a unit function is not a value class's box construction",
                    );
                    return Vec::new();
                }
                PublishedDependency::AllocatingConstructor
            } else {
                PublishedDependency::Body(singleton_dispatch)
            };
            published.push((identity.clone(), role));
        }
        for (identity, _) in &constructors {
            let Some((_, realization)) = platform.external_callable_by_dependency(identity) else {
                diags.error(
                    Span::new(0, 0),
                    "internal error: an allocating constructor has no exact provider declaration",
                );
                return Vec::new();
            };
            if realization.dependency_callable.as_ref() != Some(identity)
                || realization.kind != crate::libraries::ExternalCallableKind::Constructor
            {
                diags.error(
                    Span::new(0, 0),
                    "internal error: an allocating constructor changed its provider role",
                );
                return Vec::new();
            }
            published.push((identity.clone(), PublishedDependency::AllocatingConstructor));
        }
        if let Err(identity) = unit.record_builtin_implementation_facts(&callees) {
            diags.error(
                Span::new(0, 0),
                format!(
                    "internal error: checked KLIB body has conflicting builtin implementation facts for {identity:?}"
                ),
            );
            return Vec::new();
        }
        for (identity, constructor) in unit.initializing_constructors() {
            let Some(class_identity) = exports.classifier(constructor.class) else {
                continue;
            };
            let class = &unit.ir().classes[constructor.class as usize];
            if !class.is_abstract && !class.is_sealed {
                continue;
            }
            let Some((_, realization)) = platform.external_callable_by_dependency(identity) else {
                diags.error(
                    Span::new(0, 0),
                    "internal error: an emitted initializer has no exact provider declaration",
                );
                return Vec::new();
            };
            if realization.dependency_callable.as_ref() != Some(identity)
                || realization.kind != crate::libraries::ExternalCallableKind::Constructor
                || realization.declaration_owner
                    != Some(crate::types::SemanticCallableOwner::Classifier(
                        class.fq_name,
                    ))
                || !realization
                    .klib_body_class_contract
                    .as_ref()
                    .is_some_and(|contract| contract.matches_owner(&identity.module, class.fq_name))
            {
                diags.error(
                    Span::new(0, 0),
                    "internal error: an emitted initializer changed its constructor owner contract",
                );
                return Vec::new();
            }
            published.push((
                identity.clone(),
                PublishedDependency::ConstructorInitializer(class_identity.clone()),
            ));
        }
        let mut callables = match CheckedBackendCallables::freeze_with_class_declarations(
            unit.ir(),
            platform,
            module_facts.classes(),
        ) {
            Ok(callables) => callables,
            Err(error) => {
                diags.error(
                    Span::new(0, 0),
                    format!(
                    "internal error: cannot freeze an already-emitted KLIB support call: {error:?}"
                ),
                );
                return Vec::new();
            }
        };
        if let Some(protocol) = symbols
            .char_sequence_protocol()
            .filter(|_| backend.appends_char_sequences_by_index())
        {
            if let Err(error) = callables.freeze_char_sequence_protocol(protocol, platform) {
                diags.error(
                    Span::new(0, 0),
                    format!("internal error: cannot freeze the CharSequence members: {error:?}"),
                );
                return Vec::new();
            }
        }
        if let Err(error) = super::backend_handoff::record_checked_klib_dependency_facts(
            &mut callables,
            platform,
            &dispatched_classes,
        ) {
            diags.error(Span::new(0, 0), format!("internal error: {error}"));
            return Vec::new();
        }
        if let Err(identity) = super::builtin_implementations::record_checked(
            &mut callables,
            unit.ir(),
            crate::resolve::RepresentedOverrides::new(platform, &dispatched_classes),
        ) {
            diags.error(
                Span::new(0, 0),
                format!(
                    "internal error: checked KLIB IR has no builtin implementation facts for external member {identity:?}"
                ),
            );
            return Vec::new();
        }
        let ordinal = support_state.next_support_unit;
        support_state.next_support_unit += 1;
        let support_module = format!(
            "{}$klib_body${}${ordinal}",
            source.module_name,
            source.source.raw()
        );
        let mut support = CheckedIrFile {
            ir: unit.into_ir(),
            kind: CheckedIrUnitKind::DependencySupport,
            source: source.source,
            classifiers: CheckedBackendClassifiers::new(module_facts, platform),
            callables,
            dependency_exports: exports,
            native_plugins: symbols.native_plugins(),
            module_name: &support_module,
            stems: source.stems,
            origins: source.origins,
        };

        // Reserve only identities this concrete unit has already proved it will publish. The
        // recursive closure may link cycles to these declarations, but never treats inventory-only
        // runtime identities as if their bodies existed.
        if published.iter().any(|(identity, _)| {
            support_state.emitted.contains_key(identity)
                || support_state.in_progress.contains_key(identity)
        }) {
            diags.error(
                Span::new(0, 0),
                "internal error: one dependency body has two support-unit owners",
            );
            return Vec::new();
        }
        for (classifier, identity) in &materialized {
            if let Some(previous) = support_state
                .materialized_classes
                .get(classifier)
                .or_else(|| support_state.materializing_classes.get(classifier))
            {
                if previous == identity && value_classes.contains(classifier) {
                    // A value class's members spread over units; each one re-reads the class an
                    // earlier unit published and emits only its own members.
                    continue;
                }
                let message = if previous == identity {
                    "cannot materialize an imported class twice in separate dependency support units"
                } else {
                    "cannot materialize equal classifier names from different KLIB producers in one target program"
                };
                diags.error(Span::new(0, 0), message);
                return Vec::new();
            }
        }
        support_state.in_progress.extend(published.iter().cloned());
        support_state
            .materializing_classes
            .extend(materialized.iter().cloned());
        support_state
            .materialized_statics
            .extend(statics.iter().cloned());

        if let Some(preparation) = backend.common_ir_preparation() {
            if let Err(error) = super::backend_handoff::prepare_common_ir(
                &mut support,
                platform,
                preparation,
                symbols.coroutine_runtime(),
                symbols.enum_entries_factory(),
                &dispatched_classes,
            ) {
                release_unit_reservations(support_state, &published, &materialized, &statics);
                backend.report_common_ir_preparation_error(&error, diags);
                return Vec::new();
            }
        }

        // A common lowering may introduce real external calls (for example, continuation
        // resumption). Close those calls through the same provider-backed walk before lowering
        // this unit. Recursion emits dependencies first and sees this unit's reserved exports as
        // exact provisional owners, not as completed class layouts.
        let nested_outputs = emit_dependencies(
            &[DependencyUse {
                file: &support,
                facts: module_facts,
            }],
            symbols,
            backend,
            state,
            support_state,
            diags,
        );
        if diags.has_errors() {
            release_unit_reservations(support_state, &published, &materialized, &statics);
            return Vec::new();
        }
        let initializer_requests = match crate::backend::initializer_declarations(&support) {
            Ok(requests) => requests,
            Err(reason) => {
                release_unit_reservations(support_state, &published, &materialized, &statics);
                diags.error(Span::new(0, 0), format!("internal error: {reason}"));
                return Vec::new();
            }
        };
        let outputs = backend.lower_ir_file(support, state, diags);
        if diags.has_errors() {
            release_unit_reservations(support_state, &published, &materialized, &statics);
            return Vec::new();
        }
        for (identity, realization) in published {
            support_state.in_progress.remove(&identity);
            // An initializer the backend did not emit is not published.
            let emitted = match &realization {
                PublishedDependency::ConstructorInitializer(class) => initializer_requests
                    .iter()
                    .find(|declaration| {
                        declaration.constructor == identity && &declaration.class == class
                    })
                    .is_some_and(|declaration| {
                        backend.emitted_constructor_initializer(state, declaration)
                    }),
                _ => true,
            };
            if emitted {
                support_state.emitted.insert(identity, realization);
            }
        }
        for (classifier, identity) in materialized {
            support_state.materializing_classes.remove(&classifier);
            support_state
                .materialized_classes
                .insert(classifier, identity);
        }
        artifacts.extend(nested_outputs);
        artifacts.extend(outputs);
    }
    artifacts
}

fn release_unit_reservations(
    support_state: &mut DependencySupportState,
    published: &[(KlibDependencyCallable, PublishedDependency)],
    materialized: &[(TypeName, KlibDependencyCallable)],
    statics: &[(String, crate::metadata::klib_ir::KlibIrSymbol)],
) {
    for (identity, _) in published {
        support_state.in_progress.remove(identity);
    }
    for (classifier, _) in materialized {
        support_state.materializing_classes.remove(classifier);
    }
    for field in statics {
        support_state.materialized_statics.remove(field);
    }
}

fn rebound_externals(
    platform: &dyn crate::libraries::SemanticPlatform,
    emitted: &HashMap<KlibDependencyCallable, PublishedDependency>,
    in_progress: &HashMap<KlibDependencyCallable, PublishedDependency>,
    diags: &mut DiagSink,
) -> Option<HashMap<KlibDependencyCallable, EmittedDependencyCallable>> {
    let mut rebound =
        HashMap::with_capacity_and_hasher(emitted.len() + in_progress.len(), Default::default());
    for (identity, published) in emitted.iter().chain(in_progress) {
        let Some((external, realization)) = platform.external_callable_by_dependency(identity)
        else {
            diags.error(
                Span::new(0, 0),
                "internal error: an emitted KLIB support identity is absent from the current module provider",
            );
            return None;
        };
        if realization.dependency_callable.as_ref() != Some(identity) {
            diags.error(
                Span::new(0, 0),
                "internal error: the current module provider rebound a different KLIB support identity",
            );
            return None;
        }
        let constructor = realization.kind == crate::libraries::ExternalCallableKind::Constructor;
        let rebound_callable = match published {
            PublishedDependency::AllocatingConstructor if constructor => {
                EmittedDependencyCallable::allocating_constructor(external)
            }
            PublishedDependency::ConstructorInitializer(class_identity) if constructor => {
                let Some(crate::types::SemanticCallableOwner::Classifier(owner)) =
                    realization.declaration_owner
                else {
                    diags.error(
                        Span::new(0, 0),
                        "internal error: an emitted initializer lost its declaring classifier",
                    );
                    return None;
                };
                if !realization
                    .klib_body_class_contract
                    .as_ref()
                    .is_some_and(|contract| {
                        contract.matches_owner(&identity.module, owner)
                            && contract.module == class_identity.module
                            && contract.signature == class_identity.signature
                    })
                {
                    diags.error(Span::new(0, 0), "internal error: an emitted initializer changed its authenticated class contract");
                    return None;
                }
                EmittedDependencyCallable::constructor_initializer(external)
            }
            PublishedDependency::Body(singleton_dispatch) if !constructor => {
                EmittedDependencyCallable::new(
                    external,
                    singleton_dispatch.map(|dispatch| dispatch.ty),
                    singleton_dispatch.map(|dispatch| dispatch.classifier),
                )
            }
            _ => {
                diags.error(Span::new(0, 0), "internal error: the current module provider changed an emitted dependency role");
                return None;
            }
        };
        rebound.insert(identity.clone(), rebound_callable);
    }
    Some(rebound)
}

/// Common-operation and target-external capabilities of the same dependency emission unit.
struct DependencyLeafCapabilities<'a> {
    unit: &'a DependencyBodyUnit,
    callees: &'a KlibCalleeFacts<'a>,
    target_realizes_bodyless: bool,
    target_realizes_intrinsic: &'a dyn Fn(crate::libraries::CompilerIntrinsic) -> bool,
    target_realizes_getter: &'a dyn Fn(crate::backend::DependencyGetterSignature<'_>) -> bool,
    target_represents_classifier: &'a dyn Fn(crate::types::TypeName) -> bool,
}

impl DependencyLeafCapabilities<'_> {
    fn folds_common_builtin(&self, identity: &KlibDependencyCallable) -> bool {
        self.unit.folds_common_builtin(identity, self.callees)
    }
}

/// Prepare exact provider handles only for declaration edges actual lowering will retain.
/// A missing provider realization is an inconsistency, never permission to search or fold.
fn prepare_bodyless_externals(
    platform: &dyn crate::libraries::SemanticPlatform,
    bodies: &crate::klib_libraries::KlibDeclarationBodies,
    intrinsics: &crate::klib_libraries::KlibIntrinsicFacts,
    walked_from: impl IntoIterator<Item = KlibDependencyCallable>,
    capabilities: &DependencyLeafCapabilities<'_>,
    externals: &mut HashMap<KlibDependencyCallable, EmittedDependencyCallable>,
    diags: &mut DiagSink,
) -> bool {
    let mut pending = walked_from.into_iter().collect::<VecDeque<_>>();
    // An object a body reads is materialized with the members its initialization reaches.
    match emitted_roots::read_class_initializations(platform, bodies, capabilities.callees) {
        Ok(edges) => pending.extend(edges),
        Err(error) => {
            diags.error(Span::new(0, 0), format!("internal error: {error}"));
            return false;
        }
    }
    let mut visited = HashSet::default();
    while let Some(identity) = pending.pop_front() {
        if !visited.insert(identity.clone()) {
            continue;
        }
        if externals
            .get(&identity)
            .and_then(|external| external.constructor_initializer_target())
            .is_some()
        {
            match capabilities
                .callees
                .authenticates_retained_constructor(&identity, bodies)
            {
                Ok(true) => continue,
                Ok(false) => {
                    diags.error(
                        Span::new(0, 0),
                        "an emitted initializer lost its constructor declaration",
                    );
                    return false;
                }
                Err(reason) => {
                    diags.error(
                        Span::new(0, 0),
                        format!("invalid retained KLIB constructor declaration: {reason:?}"),
                    );
                    return false;
                }
            }
        }
        let constructor = match capabilities
            .callees
            .authenticates_constructor(&identity, bodies)
        {
            Ok(constructor) => constructor,
            Err(reason) => {
                diags.error(
                    Span::new(0, 0),
                    format!("invalid KLIB constructor declaration: {reason:?}"),
                );
                return false;
            }
        };
        let edges = if constructor {
            match capabilities
                .callees
                .constructor_initialization_callees(&identity, bodies)
            {
                Ok(edges) => edges.unwrap_or_default(),
                Err(reason) => {
                    diags.error(
                        Span::new(0, 0),
                        format!("invalid KLIB class initialization: {reason:?}"),
                    );
                    return false;
                }
            }
        } else {
            crate::klib_lowering::emitted_body(bodies, &identity).callables
        };
        let mut edges = edges;
        // The objects the body reads are materialized with the members dispatch reaches on them.
        match emitted_roots::read_object_initializations(
            platform,
            bodies,
            capabilities.callees,
            &identity,
            constructor,
        ) {
            Ok(initializations) => edges.extend(initializations),
            Err(error) => {
                diags.error(Span::new(0, 0), format!("internal error: {error}"));
                return false;
            }
        }
        // A member of a materialized class is lowered with the class its constructors create.
        match emitted_roots::member_class_constructors(
            platform,
            bodies,
            capabilities.callees,
            &identity,
        ) {
            Ok(constructors) => pending.extend(constructors),
            Err(error) => {
                diags.error(Span::new(0, 0), format!("internal error: {error}"));
                return false;
            }
        }
        // A member of a materialized object is lowered with the object its initialization
        // creates, whose edges no constructor declaration carries.
        match emitted_roots::member_object_initialization(
            platform,
            bodies,
            capabilities.callees,
            &identity,
        ) {
            Ok(Some(initialization)) => edges.extend(initialization),
            Ok(None) => {}
            Err(error) => {
                diags.error(Span::new(0, 0), format!("internal error: {error}"));
                return false;
            }
        }
        for callee in edges {
            if externals.contains_key(&callee) {
                continue;
            }
            match capabilities
                .callees
                .authenticates_constructor(&callee, bodies)
            {
                Ok(true) => {
                    let Some(external) = provider_external(platform, &callee, diags) else {
                        return false;
                    };
                    externals.insert(
                        callee.clone(),
                        EmittedDependencyCallable::constructor(external.target()),
                    );
                    pending.push_back(callee);
                    continue;
                }
                Ok(false) => {}
                Err(reason) => {
                    diags.error(
                        Span::new(0, 0),
                        format!("invalid KLIB constructor declaration: {reason:?}"),
                    );
                    return false;
                }
            }
            if capabilities.folds_common_builtin(&callee) {
                continue;
            }
            match capabilities.callees.common_primitive(&callee, bodies) {
                Ok(Some(_)) => continue,
                Ok(None) => {}
                Err(reason) => {
                    diags.error(
                        Span::new(0, 0),
                        format!("invalid common KLIB primitive: {reason:?}"),
                    );
                    return false;
                }
            }
            match capabilities.callees.continuation_intrinsic(&callee) {
                Ok(Some(_)) => continue,
                Ok(None) => {}
                Err(reason) => {
                    diags.error(
                        Span::new(0, 0),
                        format!("invalid KLIB continuation intrinsic: {reason:?}"),
                    );
                    return false;
                }
            }
            match capabilities.callees.target_intrinsic(
                &callee,
                bodies,
                capabilities.target_realizes_intrinsic,
            ) {
                Ok(Some(operation)) => {
                    let Some(external) = provider_external(platform, &callee, diags) else {
                        return false;
                    };
                    externals.insert(
                        callee,
                        EmittedDependencyCallable::target_realization(external.target(), operation),
                    );
                    continue;
                }
                Ok(None) => {}
                Err(reason) => {
                    diags.error(
                        Span::new(0, 0),
                        format!("invalid target KLIB declaration: {reason:?}"),
                    );
                    return false;
                }
            }
            if intrinsics.get(&callee).is_some() {
                // An authenticated intrinsic retains its existing provider-handle realization.
                let Some(external) = provider_external(platform, &callee, diags) else {
                    return false;
                };
                externals.insert(callee, external);
                continue;
            }
            match capabilities.callees.target_getter(
                &callee,
                bodies,
                capabilities.target_realizes_getter,
            ) {
                Ok(true) => {
                    let Some(external) = provider_external(platform, &callee, diags) else {
                        return false;
                    };
                    externals.insert(
                        callee,
                        EmittedDependencyCallable::property_getter(external.target()),
                    );
                    continue;
                }
                Ok(false) => {}
                Err(reason) => {
                    diags.error(
                        Span::new(0, 0),
                        format!("invalid target KLIB getter: {reason:?}"),
                    );
                    return false;
                }
            }
            match capabilities
                .callees
                .target_member(&callee, capabilities.target_represents_classifier)
            {
                // A member the closure froze no call edge for is one this target does not
                // realize outside its emitted bodies; lowering declines its call.
                Ok(true) if capabilities.callees.member_edge(&callee).is_none() => continue,
                Ok(true) => {
                    let Some(external) = provider_external(platform, &callee, diags) else {
                        return false;
                    };
                    externals.insert(
                        callee,
                        EmittedDependencyCallable::target_member(external.target()),
                    );
                    continue;
                }
                Ok(false) => {}
                Err(reason) => {
                    diags.error(
                        Span::new(0, 0),
                        format!("invalid target KLIB member: {reason:?}"),
                    );
                    return false;
                }
            }
            match emitted_roots::is_external_member(platform, bodies, capabilities.callees, &callee)
            {
                // A target realizing bodyless declarations realizes an abstract member of a class
                // no unit materializes from its provider handle, as it does any bodyless leaf. On
                // every target, an abstract member that dispatches on its receiver is called
                // through its provider handle, as a source call of it is.
                Ok(true)
                    if bodies.function_has_body(&callee) == Some(false)
                        && (capabilities.target_realizes_bodyless
                            || dispatches_to_an_override(platform, &callee)) => {}
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => {
                    diags.error(Span::new(0, 0), format!("internal error: {error}"));
                    return false;
                }
            }
            let Some(has_body) = bodies.function_has_body(&callee) else {
                continue;
            };
            if has_body {
                pending.push_back(callee);
                continue;
            }
            // An abstract member of a class the unit materializes is a slot of its dispatch
            // table, which every call of it dispatches through.
            match emitted_roots::member_class_constructors(
                platform,
                bodies,
                capabilities.callees,
                &callee,
            ) {
                Ok(constructors)
                    if !constructors.is_empty()
                        && bodies.function(&callee).is_some_and(|(_, function)| {
                            crate::metadata::semantic::declaration_modality(function.base.flags)
                                == crate::metadata::semantic::KotlinModality::Abstract
                        }) =>
                {
                    continue
                }
                Ok(_) => {}
                Err(error) => {
                    diags.error(Span::new(0, 0), format!("internal error: {error}"));
                    return false;
                }
            }
            // An abstract member has no body on any target: a call of it dispatches through its
            // receiver to the override the receiver's class supplies, as a source call does.
            if dispatches_to_an_override(platform, &callee) {
                let Some(external) = provider_external(platform, &callee, diags) else {
                    return false;
                };
                externals.insert(callee, external);
                continue;
            }
            if !capabilities.target_realizes_bodyless {
                diags.error(
                    Span::new(0, 0),
                    format!(
                        "internal error: the target cannot realize a bodyless KLIB support dependency: {callee:?}"
                    ),
                );
                return false;
            }
            let Some(external) = provider_external(platform, &callee, diags) else {
                return false;
            };
            externals.insert(callee, external);
        }
    }
    true
}

/// Whether the provider declares `callee` an abstract member that dispatches on its receiver.
fn dispatches_to_an_override(
    platform: &dyn crate::libraries::SemanticPlatform,
    callee: &KlibDependencyCallable,
) -> bool {
    platform
        .external_callable_by_dependency(callee)
        .is_some_and(|(_, realization)| {
            realization.kind == crate::libraries::ExternalCallableKind::Member
                && realization.callable.member_realization
                    == crate::libraries::MemberRealization::Dispatch
                && realization.callable.compiler_intrinsic.is_none()
                && realization
                    .klib_body_declaration_modifiers
                    .is_some_and(|modifiers| modifiers.declares_abstract_implementation())
        })
}

/// The provider's opaque handle for a reachable declaration support code calls without a body of
/// its own. A missing or rebound realization is an inconsistency, never a reason to search.
fn provider_external(
    platform: &dyn crate::libraries::SemanticPlatform,
    callee: &KlibDependencyCallable,
    diags: &mut DiagSink,
) -> Option<EmittedDependencyCallable> {
    let Some((external, realization)) = platform.external_callable_by_dependency(callee) else {
        crate::trace_compiler!("lower", "missing exact bodyless KLIB provider: {callee:?}");
        diags.error(
            Span::new(0, 0),
            "internal error: a bodyless KLIB dependency has no exact provider realization",
        );
        return None;
    };
    if realization.dependency_callable.as_ref() != Some(callee) {
        diags.error(
            Span::new(0, 0),
            "internal error: a bodyless KLIB dependency realization changed its exact identity",
        );
        return None;
    }
    Some(EmittedDependencyCallable::provider_leaf(external))
}

fn links_emitted_support(
    ir: &crate::ir::IrFile,
    emitted: &HashMap<KlibDependencyCallable, EmittedDependencyCallable>,
) -> bool {
    ir.exprs.iter().any(|expression| {
        let IrExpr::Call {
            callee: call_callee,
            ..
        } = expression
        else {
            return false;
        };
        let Callee::External { target, .. } = &**call_callee else {
            return false;
        };
        emitted.values().any(|emitted| emitted.target() == *target)
    })
}

#[cfg(test)]
mod tests {
    use super::DependencySupportState;
    use crate::libraries::{
        DependencyBodyProviderRole, DependencyBodyProviderSnapshot, DependencyBodySnapshot,
    };

    fn provider(
        archive: &str,
        module: &str,
        compiler_version: &str,
        role: DependencyBodyProviderRole,
    ) -> DependencyBodyProviderSnapshot {
        DependencyBodyProviderSnapshot::new(
            archive.into(),
            module,
            Some("2.4.0"),
            Some(compiler_version),
            Some("2.4.0"),
            [compiler_version.as_bytes()[0]; 32],
            role,
        )
    }

    #[test]
    fn one_target_session_rejects_a_different_klib_provider_snapshot() {
        let first = DependencyBodySnapshot::new(vec![provider(
            "/toolchain/stdlib",
            "stdlib",
            "2.4.10",
            DependencyBodyProviderRole::PlatformWasm,
        )]);
        let same_inputs_another_compilation = first.clone();
        let independently_opened_inputs = DependencyBodySnapshot::new(vec![provider(
            "/toolchain/stdlib",
            "stdlib",
            "2.4.20",
            DependencyBodyProviderRole::PlatformWasm,
        )]);
        let mut state = DependencySupportState::default();

        assert!(state.accepts_snapshot(&first));
        assert!(state.accepts_snapshot(&same_inputs_another_compilation));
        assert!(!state.accepts_snapshot(&independently_opened_inputs));
        assert!(state.accepts_snapshot(&first));
    }

    #[test]
    fn a_target_session_binds_every_transitive_body_provider_independent_of_input_order() {
        let stdlib = provider(
            "/toolchain/stdlib",
            "stdlib",
            "2.4.20",
            DependencyBodyProviderRole::PlatformWasm,
        );
        let dependency = provider(
            "/toolchain/dependency",
            "dependency",
            "2.4.20",
            DependencyBodyProviderRole::Dependency,
        );
        let first = DependencyBodySnapshot::new(vec![stdlib.clone(), dependency.clone()]);
        let reverse_input_order = DependencyBodySnapshot::new(vec![dependency, stdlib.clone()]);
        let changed_dependency = DependencyBodySnapshot::new(vec![
            stdlib,
            provider(
                "/toolchain/dependency",
                "dependency",
                "2.4.21",
                DependencyBodyProviderRole::Dependency,
            ),
        ]);
        let mut state = DependencySupportState::default();

        assert!(state.accepts_snapshot(&first));
        assert!(state.accepts_snapshot(&reverse_input_order));
        assert!(!state.accepts_snapshot(&changed_dependency));
    }
}
