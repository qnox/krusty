//! An authenticated, bounded ordinary-class initialization capability.
//!
//! Planning and lowering use this same declaration-owned plan. Fields come from actual serialized
//! members; constructor parameters never imply storage. Other class representations remain target
//! constructor leaves, while an admitted class with invalid initialization declines transactionally.

use super::class_headers::{self, ClassMemberHeader};
use super::decline::KlibBodyDeclineReason;
use super::klib_types::semantic_type;
use crate::fx_hash::HashSet;
use crate::klib_libraries::{KlibCalleeOrigin, KlibDeclarationBodies};
use crate::libraries::{
    KlibBodyCallable, KlibClassStorage, KlibDeclarationSignature, KlibDependencyCallable, TypeKind,
};
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::tree::{
    KlibIrArguments, KlibIrBody, KlibIrExprId, KlibIrExprKind, KlibIrField, KlibIrFunction,
    KlibIrMember, KlibIrStatement,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbolKind};
use crate::metadata::semantic::{class_kind, KotlinModality};
use crate::types::Ty;

pub(super) enum InitializationStep<'a> {
    Field { index: u32, value: KlibIrExprId },
    Block(&'a [KlibIrStatement]),
}

/// One member's contribution to initialization, in member order. A field store is the field's own
/// initializer expression; an anonymous initializer is its statement list. Constructor parameter
/// names never create a store.
pub(super) fn record_initializer<'a>(
    member: &'a KlibIrMember,
    field: Option<(u32, &'a KlibIrField)>,
    steps: &mut Vec<InitializationStep<'a>>,
) -> Result<(), KlibBodyDeclineReason> {
    if let KlibIrMember::AnonymousInitializer { base, statements } = member {
        if base.flags != 0 {
            return Err(KlibBodyDeclineReason::UnsupportedOperation(
                "a static or flagged class initializer",
            ));
        }
        steps.push(InitializationStep::Block(statements));
    }
    if let Some((index, field)) = field {
        if let Some(value) = field.initializer {
            steps.push(InitializationStep::Field { index, value });
        }
    }
    Ok(())
}

pub(super) struct ClassInitialization<'a, 'b> {
    pub(super) header: ClassMemberHeader<'a, 'b>,
    pub(super) field_declarations: Vec<&'b KlibIrField>,
    pub(super) storage: KlibClassStorage,
    pub(super) steps: Vec<InitializationStep<'b>>,
    /// The members dispatch on an instance of the class reaches: its overrides with bodies, and
    /// the open and abstract members an open class declares. A materialized class always carries
    /// them, so dispatch never reaches an implementation the instance's class overrides.
    pub(super) dispatched: Vec<&'b KlibIrFunction>,
    /// The default accessors of its stored properties another declaration can call, which no
    /// dispatch reaches. The unit materializing the class owns its storage, so it emits them
    /// with the class: a later unit calls their exports instead of materializing it again.
    pub(super) storage_accessors: Vec<&'b KlibIrFunction>,
    /// The superclass constructor the constructor delegates to, unless that is `Any`'s.
    pub(super) superclass: Option<SuperclassCall>,
    /// Whether the constructor is the class's primary one. A class without a primary constructor
    /// is initialized by each of its secondary constructors, which all delegate to the superclass.
    pub(super) primary: bool,
    /// Every declared supertype, the superclass first.
    pub(super) supertypes: Vec<Ty>,
    /// An enum class's entries, in declaration order; none for any other class.
    pub(super) entries: Vec<super::enum_classes::EnumEntryInitialization>,
}

/// The delegation of a constructor to its superclass's constructor.
pub(super) struct SuperclassCall {
    pub(super) constructor: KlibDependencyCallable,
    /// The delegation's arguments, in the superclass constructor's parameter order; none for
    /// each parameter it leaves to its default.
    pub(super) arguments: Vec<Option<KlibIrExprId>>,
    /// The superclass, and the parameters its selected constructor declares.
    pub(super) owner: crate::types::TypeName,
    pub(super) params: Vec<Ty>,
    /// Whether the unit materializes the superclass from its own admitted initialization. A
    /// superclass the target lays out itself is the target's, as `Any` is.
    pub(super) materialized: bool,
}

fn mismatch(detail: &str) -> KlibBodyDeclineReason {
    KlibBodyDeclineReason::SignatureMismatch(detail.to_owned())
}

/// A positive common representation, independently of a target's runtime constructor registry.
pub(super) fn plan<'a, 'b>(
    selected: KlibBodyCallable<'a>,
    bodies: &'b KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
) -> Result<Option<ClassInitialization<'a, 'b>>, KlibBodyDeclineReason> {
    let header = class_headers::authenticate(selected, bodies)?;
    if header
        .contract
        .primary_constructor
        .as_ref()
        .is_some_and(|primary| {
            selected.identity().declaration != KlibDeclarationSignature::Public(primary.clone())
        })
    {
        return Ok(None);
    }
    admit(header, TypeKind::Class, bodies, callees)
}

/// The initialization of `class`, an object or an enum class, through its private primary
/// constructor, which no provider records: the class's own declaration is the only creator of its
/// instances, the object's one or the enum's entries.
pub(super) fn declared_instances_plan<'a, 'b>(
    class: &'a crate::libraries::KlibBodyClassContract,
    bodies: &'b KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
) -> Result<Option<ClassInitialization<'a, 'b>>, KlibBodyDeclineReason> {
    match class_headers::authenticate_declared_instances_constructor(class, bodies)? {
        Some(header) => admit(header, class.kind, bodies, callees),
        None => Ok(None),
    }
}

/// The initialization of a class of `kind` by the constructor `header` authenticates.
fn admit<'a, 'b>(
    header: ClassMemberHeader<'a, 'b>,
    kind: TypeKind,
    bodies: &'b KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
) -> Result<Option<ClassInitialization<'a, 'b>>, KlibBodyDeclineReason> {
    let contract = header.contract;
    // A class the target lays out itself is the target's to construct, as `Any` is.
    if contract.kind != kind
        || callees.target_represents(contract.classifier)
        || !matches!(
            contract.modality,
            KotlinModality::Final | KotlinModality::Open | KotlinModality::Abstract
        )
        || contract.is_value
        || contract.is_expect
        || header.class.value_class_representation.is_some()
    {
        return Ok(None);
    }
    let open = contract.modality != KotlinModality::Final;
    let mut constructors = Vec::new();
    let mut dispatched = Vec::new();
    let mut storage_accessors = Vec::new();
    // A member function or property accessor dispatch reaches: an override with a body, an open
    // class's open member, or an abstract member. A fake override is the inherited declaration;
    // any other bodyless member is the target's, and the class keeps that target boundary.
    let mut dispatch = |function: &'b KlibIrFunction| -> bool {
        let modality = crate::metadata::semantic::declaration_modality(function.base.flags);
        if function.body.is_none() {
            match modality {
                _ if function.base.origin == "FAKE_OVERRIDE" => {}
                KotlinModality::Abstract => dispatched.push(function),
                // A member bound to a runtime entry is realized there, so it does not keep its
                // class on the target's side of the boundary.
                _ if crate::klib_libraries::is_runtime_entry_ir(header.arena, function) => {}
                _ => return false,
            }
        } else if !function.overridden.is_empty() || (open && modality != KotlinModality::Final) {
            dispatched.push(function);
        }
        true
    };
    for member in &header.class.members {
        match member {
            KlibIrMember::Function(function) => {
                let function = header.arena.function(*function);
                if function.constructor {
                    constructors.push(&function.base.symbol);
                } else if !dispatch(function) {
                    return Ok(None);
                }
            }
            KlibIrMember::Property(property) => {
                for accessor in [property.getter, property.setter].into_iter().flatten() {
                    let accessor = header.arena.function(accessor);
                    if !dispatch(accessor) {
                        return Ok(None);
                    }
                    if property.backing_field.is_some()
                        && accessor.body.is_some()
                        && accessor.base.origin
                            == super::function_lowering::DEFAULT_PROPERTY_ACCESSOR
                        && accessor.overridden.is_empty()
                        && (!open
                            || crate::metadata::semantic::declaration_modality(accessor.base.flags)
                                == KotlinModality::Final)
                        && crate::metadata::semantic::declaration_visibility(accessor.base.flags)
                            != crate::types::Visibility::Private
                    {
                        storage_accessors.push(accessor);
                    }
                }
            }
            // A nested class, a companion object included, is a class of its own. An enum
            // class's entries are its instances, which the entries' own authentication reads.
            KlibIrMember::Field(_)
            | KlibIrMember::Class(_)
            | KlibIrMember::AnonymousInitializer { .. } => {}
            KlibIrMember::EnumEntry(_) if kind == TypeKind::Enum => {}
            _ => return Ok(None),
        }
    }
    // A materialized class carries every member dispatch reaches, so a class whose members the
    // unit does not emit with it keeps its target boundary rather than losing its overrides.
    if !dispatched.is_empty() && !contract.emits_members() {
        return Ok(None);
    }
    let primary = contract.primary_constructor.is_some();
    // A class with a primary constructor is initialized by that one alone: each of its secondary
    // constructors delegates to it (`own_delegation`). Without one, the selected secondary
    // constructor initializes the class.
    if !constructors.contains(&&header.function.base.symbol)
        || header
            .function
            .regular_parameters
            .iter()
            .any(|parameter| parameter.vararg_element_type.is_some())
    {
        return Ok(None);
    }
    let supertypes = header
        .class
        .supertypes
        .iter()
        .map(|&supertype| semantic_type(header.arena, &header.types, supertype))
        .collect::<Result<Vec<_>, _>>()?;
    let Some(KlibIrBody::Block(statements)) = &header.function.body else {
        // A bodyless or synthetic constructor retains its separately authenticated target boundary.
        return Ok(None);
    };
    let receiver = header
        .class
        .this_receiver
        .as_ref()
        .ok_or_else(|| mismatch("a materialized class has no serialized instance receiver"))?;
    if semantic_type(header.arena, &header.types, receiver.ty)? != contract.self_type() {
        return Err(mismatch(
            "a class instance receiver has another declared type",
        ));
    }
    // The delegation and instance initialization, then the rest of the constructor's own body,
    // which runs after every member initializer as a secondary constructor's body does.
    let [KlibIrStatement::Expression(delegate), KlibIrStatement::Expression(initialize), body @ ..] =
        statements.as_slice()
    else {
        // A constructor body without its delegation and instance initialization keeps its
        // separately authenticated target boundary, as a bodyless one does.
        return Ok(None);
    };
    // An enum class's constructor delegates to `kotlin.Enum`'s, which the target lays out.
    let (delegation, entries) = if kind == TypeKind::Enum {
        // An entry leaves a defaulted parameter to its default, which is lowered in the call
        // frame of a constructor call, not in the entry's initializer frame: such an enum keeps
        // its target boundary.
        if header
            .function
            .regular_parameters
            .iter()
            .any(|parameter| parameter.default_value.is_some())
        {
            return Ok(None);
        }
        if !matches!(&header.arena.expr(*initialize).kind,
            KlibIrExprKind::InstanceInitializerCall(class) if class == &header.class.base.symbol)
        {
            return Err(mismatch(
                "an enum constructor does not initialize its class after its delegation",
            ));
        }
        let (superclass, declared) =
            super::enum_classes::kotlin_enum_delegation(&header, bodies, callees, *delegate)?;
        let delegation = SuperclassDelegation {
            superclass: Some(superclass),
            declared,
            root: false,
        };
        (delegation, super::enum_classes::entries(&header)?)
    } else {
        match superclass_delegation(&header, bodies, callees, delegate, initialize)? {
            Some(delegation) => (delegation, Vec::new()),
            None => return Ok(None),
        }
    };
    let SuperclassDelegation {
        superclass,
        declared: declared_superclass,
        root,
    } = delegation;
    // Kotlin lists supertypes in any order (`ArrayList` names `MutableList` before
    // `AbstractMutableList`). The class the constructor delegates to is one of them, or `Any`,
    // which a class extending no other class may leave implicit; it leads the class's supertypes.
    if !root && !supertypes.contains(&declared_superclass) {
        return Err(mismatch(
            "a class's supertypes do not name the class its constructor delegates to",
        ));
    }
    let mut steps = Vec::new();
    let storage =
        super::class_storage::authenticate(&header, declared_superclass, |member, field| {
            record_initializer(member, field, &mut steps)
        })?;
    if !body.is_empty() {
        steps.push(InitializationStep::Block(body));
    }
    // The class lists its supertypes in source order; the class it constructs leads them.
    let mut supertypes = supertypes;
    supertypes.retain(|supertype| *supertype != declared_superclass);
    supertypes.insert(0, declared_superclass);
    Ok(Some(ClassInitialization {
        header,
        field_declarations: storage.field_declarations,
        storage: storage.storage,
        steps,
        dispatched,
        storage_accessors,
        superclass,
        primary,
        supertypes,
        entries,
    }))
}

/// How an ordinary class constructor delegates to its superclass's.
struct SuperclassDelegation {
    /// The superclass constructor call, unless it is `Any`'s.
    superclass: Option<SuperclassCall>,
    /// The superclass, as the class's supertypes name it.
    declared: Ty,
    /// Whether the delegation is to `Any`'s constructor.
    root: bool,
}

/// The delegation `delegate` of the ordinary class constructor `header` to its superclass's,
/// followed by the instance initialization `initialize`. `None` when the class keeps its target
/// boundary.
fn superclass_delegation(
    header: &ClassMemberHeader<'_, '_>,
    bodies: &KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
    delegate: &KlibIrExprId,
    initialize: &KlibIrExprId,
) -> Result<Option<SuperclassDelegation>, KlibBodyDeclineReason> {
    let contract = header.contract;
    let KlibIrExprKind::DelegatingConstructorCall(access) = &header.arena.expr(*delegate).kind
    else {
        return Err(mismatch(
            "a primary constructor does not delegate before initialization",
        ));
    };
    if access.symbol.kind != KlibIrSymbolKind::Constructor
        || !matches!(&header.arena.expr(*initialize).kind,
            KlibIrExprKind::InstanceInitializerCall(class) if class == &header.class.base.symbol)
    {
        let initialized = match &header.arena.expr(*initialize).kind {
            KlibIrExprKind::InstanceInitializerCall(class) => {
                format!("instance initializer for {:?}", class.signature)
            }
            other => format!("initializer expression {other:?}"),
        };
        return Err(mismatch(&format!(
            "ordinary KLIB class {:?} has invalid superclass initialization: delegation {:?} (kind {:?}, {} type arguments), {initialized}; expected a constructor paired with an initializer for {:?}",
            contract.signature,
            access.symbol.signature,
            access.symbol.kind,
            access.type_arguments.len(),
            header.class.base.symbol.signature,
        )));
    }
    // The superclass's type arguments, as the class's first supertype applies them.
    let type_arguments = access
        .type_arguments
        .iter()
        .map(|argument| {
            let argument = argument.ok_or_else(|| {
                mismatch("a superclass constructor delegation omits a type argument")
            })?;
            semantic_type(header.arena, &header.types, argument)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let applied = |owner| {
        if type_arguments.is_empty() {
            Ty::obj_name(owner)
        } else {
            Ty::obj_args_name(owner, &type_arguments)
        }
    };
    let KlibIrSignature::Public(signature) = &access.symbol.signature else {
        return Ok(None);
    };
    let arguments = match &access.arguments {
        KlibIrArguments::Flat(values) => values.clone(),
        KlibIrArguments::Split {
            dispatch_receiver: None,
            extension_receiver: None,
            values,
        } => values.clone(),
        KlibIrArguments::Split { .. } => {
            return Err(mismatch(
                "a superclass constructor delegation passes a receiver",
            ))
        }
    };
    let root = super::ir_builtins::root_constructor(signature, || {
        callees.validate_root_constructor(&contract.module, signature, bodies)
    })?;
    let (superclass, declared_superclass) = if root {
        if !arguments.is_empty() || !type_arguments.is_empty() {
            return Err(mismatch(
                "a delegation to `Any`'s constructor passes arguments or type arguments",
            ));
        }
        (None, Ty::obj_name(crate::types::wk::any()))
    } else {
        // The superclass constructor the delegation names, by its exact declaration.
        let constructor_identity = match bodies.linked_origin(
            &contract.module,
            &KlibDeclarationSignature::Public(signature.clone()),
        ) {
            KlibCalleeOrigin::Exact(identity) => identity,
            KlibCalleeOrigin::Missing | KlibCalleeOrigin::Ambiguous => {
                return Err(mismatch(
                    "a superclass constructor delegation names no unique declaration",
                ))
            }
        };
        let Some(super::callee_facts::KlibCalleeFact::Selected(selected)) =
            callees.get(&constructor_identity)
        else {
            return Ok(None);
        };
        let Some(crate::types::SemanticCallableOwner::Classifier(owner)) =
            selected[0].declaration_owner()
        else {
            return Err(mismatch("a superclass constructor has no declaring class"));
        };
        let constructor = super::constructor_calls::authenticate(selected[0], bodies)?;
        if constructor.classifier != owner {
            return Err(mismatch(
                "a superclass constructor's authenticated class disagrees with its owner",
            ));
        }
        if arguments.len() != constructor.declared_parameters().len()
            || arguments.iter().enumerate().any(|(ordinal, argument)| {
                argument.is_none() && !constructor.default_allowed(ordinal)
            })
        {
            return Err(mismatch(
                "a superclass constructor delegation omits a parameter without a declared default",
            ));
        }
        let applied_params =
            constructor.instantiate(&type_arguments, applied(owner), |argument, bound| {
                callees.constructor_bound(&header.types, argument, bound)
            })?;
        if callees.target_represents(owner) {
            // The target lays the superclass out itself and builds it from the delegation's
            // arguments, as it does for a source class extending it. Its defaults are the
            // target's, so a delegation leaving one out keeps the boundary.
            if arguments.iter().any(Option::is_none) {
                return Ok(None);
            }
            (
                Some(SuperclassCall {
                    constructor: constructor_identity,
                    arguments,
                    owner,
                    params: applied_params,
                    materialized: false,
                }),
                applied(owner),
            )
        } else {
            let Some(superclass) = plan(selected[0], bodies, callees)? else {
                return Ok(None);
            };
            if superclass.header.contract.modality == KotlinModality::Final {
                return Err(mismatch("a class extends a final class"));
            }
            (
                Some(SuperclassCall {
                    constructor: constructor_identity,
                    arguments,
                    owner: superclass.header.contract.classifier,
                    params: applied_params,
                    materialized: true,
                }),
                applied(superclass.header.contract.classifier),
            )
        }
    };
    Ok(Some(SuperclassDelegation {
        superclass,
        declared: declared_superclass,
        root,
    }))
}

/// A secondary constructor of a class with a primary one: it delegates to another constructor
/// of its class, the primary one or a secondary one reaching it the same way, then runs its own
/// statements. The primary constructor's admitted initialization builds the instance.
pub(super) struct OwnDelegation<'a, 'b> {
    /// The secondary constructor's own header; its parameters are the constructor's.
    pub(super) header: ClassMemberHeader<'a, 'b>,
    /// The constructor of the class it delegates to, and the declaration selected for it.
    pub(super) target: KlibDependencyCallable,
    pub(super) target_selected: KlibBodyCallable<'a>,
    /// The delegation's arguments, in the target constructor's parameter order; none for each
    /// parameter it leaves to its default.
    pub(super) arguments: Vec<Option<KlibIrExprId>>,
    /// The statements the constructor runs after the delegation.
    pub(super) statements: &'b [KlibIrStatement],
}

/// The delegation of the secondary constructor `selected` to another constructor of its class,
/// when the chain of such delegations reaches the primary constructor and the class's primary
/// initialization is admitted. Any other secondary constructor keeps its target boundary.
pub(super) fn own_delegation<'a, 'b>(
    selected: KlibBodyCallable<'a>,
    bodies: &'b KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'a>,
) -> Result<Option<OwnDelegation<'a, 'b>>, KlibBodyDeclineReason> {
    let mut visited = HashSet::default();
    own_delegation_from(selected, bodies, callees, &mut visited)
}

/// [`own_delegation`], over the secondary constructors `visited` on the way to `selected`. Kotlin
/// rejects a cyclic delegation, so a serialized one is a malformed declaration.
fn own_delegation_from<'a, 'b>(
    selected: KlibBodyCallable<'a>,
    bodies: &'b KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'a>,
    visited: &mut HashSet<KlibDependencyCallable>,
) -> Result<Option<OwnDelegation<'a, 'b>>, KlibBodyDeclineReason> {
    if !visited.insert(selected.identity().clone()) {
        return Err(mismatch("a secondary constructor delegation is cyclic"));
    }
    let header = class_headers::authenticate(selected, bodies)?;
    let contract = header.contract;
    let Some(primary_signature) = &contract.primary_constructor else {
        return Ok(None);
    };
    let primary = KlibDependencyCallable {
        module: contract.module.clone(),
        declaration: KlibDeclarationSignature::Public(primary_signature.clone()),
    };
    if selected.identity() == &primary
        || header
            .function
            .regular_parameters
            .iter()
            .any(|parameter| parameter.vararg_element_type.is_some())
    {
        return Ok(None);
    }
    let Some(KlibIrBody::Block(statements)) = &header.function.body else {
        return Ok(None);
    };
    let Some((KlibIrStatement::Expression(delegate), statements)) = statements.split_first() else {
        return Err(mismatch("a secondary constructor does not delegate"));
    };
    let KlibIrExprKind::DelegatingConstructorCall(access) = &header.arena.expr(*delegate).kind
    else {
        return Err(mismatch(
            "a secondary constructor does not delegate before its own statements",
        ));
    };
    // A delegation to a superclass keeps the boundary. A constructor of the class shares the
    // primary constructor's package and declaration path, and differs only in its member id.
    let KlibIrSignature::Public(target_signature) = &access.symbol.signature else {
        return Ok(None);
    };
    if access.symbol.kind != KlibIrSymbolKind::Constructor
        || target_signature.package() != primary_signature.package()
        || target_signature.declaration() != primary_signature.declaration()
    {
        return Ok(None);
    }
    let target = KlibDependencyCallable {
        module: contract.module.clone(),
        declaration: KlibDeclarationSignature::Public(target_signature.clone()),
    };
    // A delegation within the class applies the class's own formals. An inner class's delegation
    // passes only its own; the outer class's ones it captures are the same outer instance's.
    let type_arguments = access
        .type_arguments
        .iter()
        .map(|argument| {
            let argument = argument
                .ok_or_else(|| mismatch("a delegation within the class omits a type argument"))?;
            semantic_type(header.arena, &header.types, argument)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let own_formals = contract
        .parameters
        .formals
        .get(..contract.parameters.own_count)
        .ok_or_else(|| mismatch("a class lists more own type parameters than formals"))?;
    if *type_arguments != *own_formals {
        return Err(mismatch(
            "a delegation within the class applies other type arguments than the class's own",
        ));
    }
    // An inner class's delegation forwards the outer instance first, as its constructors take it.
    let inner = header.outer.is_some();
    let arguments = super::constructor_calls::constructor_arguments(access, inner)?;
    let Some(super::callee_facts::KlibCalleeFact::Selected(target_selected)) = callees.get(&target)
    else {
        return Ok(None);
    };
    let target_selected = target_selected[0];
    let defaults = target_selected.parameter_defaults().ok_or_else(|| {
        mismatch("a delegation within the class reaches a constructor without declared defaults")
    })?;
    if arguments.len() != usize::from(inner) + defaults.len()
        || arguments
            .iter()
            .zip(std::iter::repeat_n(&false, usize::from(inner)).chain(defaults.iter()))
            .any(|(argument, default)| argument.is_none() && !default)
    {
        return Err(mismatch(
            "a delegation within the class omits a parameter without a declared default",
        ));
    }
    let admitted = if target == primary {
        plan(target_selected, bodies, callees)?.is_some()
    } else {
        own_delegation_from(target_selected, bodies, callees, visited)?.is_some()
    };
    if !admitted {
        return Ok(None);
    }
    Ok(Some(OwnDelegation {
        header,
        target,
        target_selected,
        arguments,
        statements,
    }))
}

/// Exact edges of the same admitted initialization which lowering will execute.
pub(super) fn constructor_initialization_callees(
    selected: KlibBodyCallable<'_>,
    bodies: &KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
) -> Result<Option<Vec<KlibDependencyCallable>>, KlibBodyDeclineReason> {
    match plan(selected, bodies, callees)? {
        Some(plan) => Ok(Some(initialization_callees(&plan, bodies, callees))),
        None => own_delegation_callees(selected, bodies, callees),
    }
}

/// The library objects the same admitted initialization reads as values.
pub(super) fn constructor_initialization_objects(
    selected: KlibBodyCallable<'_>,
    bodies: &KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
) -> Result<Vec<KlibPublicIdSignature>, KlibBodyDeclineReason> {
    Ok(plan(selected, bodies, callees)?
        .map(|plan| initialization_objects(&plan, bodies))
        .unwrap_or_default())
}

/// The library objects and enum classes the admitted initialization of `class`, an object or an
/// enum class, reads as values.
pub(super) fn declared_instances_objects(
    class: &crate::libraries::KlibBodyClassContract,
    bodies: &KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
) -> Result<Vec<KlibPublicIdSignature>, KlibBodyDeclineReason> {
    Ok(declared_instances_plan(class, bodies, callees)?
        .map(|plan| initialization_objects(&plan, bodies))
        .unwrap_or_default())
}

/// Exact edges of the admitted initialization of `class`, an object or an enum class.
pub(super) fn declared_instances_callees(
    class: &crate::libraries::KlibBodyClassContract,
    bodies: &KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
) -> Result<Option<Vec<KlibDependencyCallable>>, KlibBodyDeclineReason> {
    Ok(declared_instances_plan(class, bodies, callees)?
        .map(|plan| initialization_callees(&plan, bodies, callees)))
}

/// The expressions an initialization evaluates: the constructor's parameter defaults, which its
/// default-argument entry runs, an enum's entry arguments, the superclass constructor's
/// arguments, then each field initializer and initializer block.
fn initialization_roots(plan: &ClassInitialization<'_, '_>) -> Vec<KlibIrExprId> {
    let mut roots = parameter_defaults(plan.header.function);
    roots.extend(
        plan.entries
            .iter()
            .flat_map(|entry| entry.arguments.iter().copied()),
    );
    if let Some(superclass) = &plan.superclass {
        roots.extend(superclass.arguments.iter().flatten().copied());
    }
    for step in &plan.steps {
        match step {
            InitializationStep::Field { value, .. } => roots.push(*value),
            InitializationStep::Block(statements) => {
                roots.extend(plan.header.arena.statement_expression_roots(statements));
            }
        }
    }
    roots
}

/// The defaults of a constructor's parameters, which its default-argument entry evaluates.
fn parameter_defaults(constructor: &KlibIrFunction) -> Vec<KlibIrExprId> {
    constructor
        .regular_parameters
        .iter()
        .filter_map(|parameter| parameter.default_value)
        .collect()
}

/// The library classes whose declared instances an initialization reads as values: each object
/// it reads, and the enum class of each entry it reads. A dispatched member's body is walked as
/// the member is.
fn initialization_objects(
    plan: &ClassInitialization<'_, '_>,
    bodies: &KlibDeclarationBodies,
) -> Vec<KlibPublicIdSignature> {
    let (arena, roots) = (plan.header.arena, initialization_roots(plan));
    let mut objects: Vec<KlibPublicIdSignature> = Vec::new();
    for symbol in arena.emitted_object_symbols_from(roots.clone(), super::EMITTED_CALL_FORMS) {
        if let KlibIrSignature::Public(signature) = &symbol.signature {
            if !objects.contains(signature) {
                objects.push(signature.clone());
            }
        }
    }
    let entries = arena.emitted_enum_entry_symbols_from(roots, super::EMITTED_CALL_FORMS);
    for class in bodies.enum_entry_classes(&plan.header.contract.module, entries) {
        if !objects.contains(&class) {
            objects.push(class);
        }
    }
    objects
}

/// The checked declaration the synthetic `entries` body of the enum `plan` initializes calls:
/// Kotlin builds an enum's `entries` by passing its `values()` to the selected `enumEntries`. A
/// class without that body has no such edge.
pub(super) fn entries_factory<'c>(
    plan: &ClassInitialization<'_, '_>,
    callees: &'c super::KlibCalleeFacts<'_>,
) -> Option<&'c KlibDependencyCallable> {
    let synthetic_entries = plan
        .header
        .class
        .declares_synthetic_enum_entries(plan.header.arena);
    callees.enum_entries_factory().filter(|_| synthetic_entries)
}

fn initialization_callees(
    plan: &ClassInitialization<'_, '_>,
    bodies: &KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
) -> Vec<KlibDependencyCallable> {
    let roots = initialization_roots(plan);
    let superclass = plan
        .superclass
        .as_ref()
        .filter(|superclass| superclass.materialized)
        .map(|superclass| superclass.constructor.clone());
    plan.header
        .arena
        .emitted_callable_symbols_from(roots, super::EMITTED_CALL_FORMS)
        .into_iter()
        // The class's dispatched members are always emitted with it, as dispatch reaches them,
        // and so are the accessors of the storage it owns.
        .chain(
            plan.dispatched
                .iter()
                .chain(&plan.storage_accessors)
                .map(|function| &function.base.symbol),
        )
        .filter_map(|symbol| match &symbol.signature {
            KlibIrSignature::Public(signature) => {
                Some(KlibDeclarationSignature::Public(signature.clone()))
            }
            KlibIrSignature::Accessor(signature) => {
                Some(KlibDeclarationSignature::Accessor(signature.clone()))
            }
            _ => None,
        })
        .filter_map(|signature| {
            match bodies.linked_origin(&plan.header.contract.module, &signature) {
                KlibCalleeOrigin::Exact(identity) => Some(identity),
                KlibCalleeOrigin::Missing | KlibCalleeOrigin::Ambiguous => None,
            }
        })
        // A materialized superclass is initialized first, through its own constructor.
        .chain(superclass)
        .chain(entries_factory(plan, callees).cloned())
        .collect()
}

/// Exact edges of a secondary constructor delegating to its class's primary constructor: the
/// primary constructor, which initializes the instance, and the callees of the delegation's
/// arguments and the constructor's own statements.
fn own_delegation_callees(
    selected: KlibBodyCallable<'_>,
    bodies: &KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
) -> Result<Option<Vec<KlibDependencyCallable>>, KlibBodyDeclineReason> {
    let Some(delegation) = own_delegation(selected, bodies, callees)? else {
        return Ok(None);
    };
    let arena = delegation.header.arena;
    let roots = parameter_defaults(delegation.header.function)
        .into_iter()
        .chain(delegation.arguments.iter().flatten().copied())
        .chain(arena.statement_expression_roots(delegation.statements))
        .collect();
    Ok(Some(
        arena
            .emitted_callable_symbols_from(roots, super::EMITTED_CALL_FORMS)
            .into_iter()
            .filter_map(|symbol| crate::klib_libraries::member_signature(&symbol.signature))
            .filter_map(|signature| {
                match bodies.linked_origin(&delegation.header.contract.module, &signature) {
                    KlibCalleeOrigin::Exact(identity) => Some(identity),
                    KlibCalleeOrigin::Missing | KlibCalleeOrigin::Ambiguous => None,
                }
            })
            .chain(std::iter::once(delegation.target))
            .collect(),
    ))
}

/// The constructors that initialize an instance of the ordinary class `class`: its primary
/// constructor, or each of its constructors when it has no primary one. A secondary constructor of
/// a class with a primary one delegates to it and initializes nothing itself. `None` when the
/// class's serialized constructors are not each one public declaration.
pub(crate) fn initializing_constructors(
    class: &crate::libraries::KlibBodyClassContract,
    bodies: &KlibDeclarationBodies,
) -> Option<Vec<KlibDependencyCallable>> {
    let constructors = match &class.primary_constructor {
        Some(primary) => vec![primary.clone()],
        None => bodies.public_class_constructors(&class.module, &class.signature)?,
    };
    Some(
        constructors
            .into_iter()
            .map(|constructor| KlibDependencyCallable {
                module: class.module.clone(),
                declaration: KlibDeclarationSignature::Public(constructor),
            })
            .collect(),
    )
}

/// The superclass constructor the primary constructor `identity` serializes a delegation to,
/// read from its body alone. Admitting the class's initialization needs that constructor's own
/// declaration, so a closure records it before asking.
pub(crate) fn delegated_superclass_constructor(
    bodies: &KlibDeclarationBodies,
    identity: &KlibDependencyCallable,
) -> Option<KlibDependencyCallable> {
    let (arena, function) = bodies.function(identity)?;
    if !function.constructor {
        return None;
    }
    let Some(KlibIrBody::Block(statements)) = &function.body else {
        return None;
    };
    let Some(KlibIrStatement::Expression(delegate)) = statements.first() else {
        return None;
    };
    let KlibIrExprKind::DelegatingConstructorCall(access) = &arena.expr(*delegate).kind else {
        return None;
    };
    let KlibIrSignature::Public(signature) = &access.symbol.signature else {
        return None;
    };
    match bodies.linked_origin(
        &identity.module,
        &KlibDeclarationSignature::Public(signature.clone()),
    ) {
        KlibCalleeOrigin::Exact(constructor) => Some(constructor),
        KlibCalleeOrigin::Missing | KlibCalleeOrigin::Ambiguous => None,
    }
}

/// The members of an admitted class that dispatch reaches, which are emitted with the class
/// whatever a call of one would fold to: a virtual call runs the member's own body.
pub(super) fn constructor_dispatched_members(
    selected: KlibBodyCallable<'_>,
    bodies: &KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
) -> Result<Vec<KlibDependencyCallable>, KlibBodyDeclineReason> {
    match plan(selected, bodies, callees)? {
        Some(plan) => dispatched_members(&plan, bodies),
        None => Ok(Vec::new()),
    }
}

/// The members dispatch reaches of the admitted object or enum class `class`.
pub(super) fn declared_instances_dispatched_members(
    class: &crate::libraries::KlibBodyClassContract,
    bodies: &KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
) -> Result<Vec<KlibDependencyCallable>, KlibBodyDeclineReason> {
    match declared_instances_plan(class, bodies, callees)? {
        Some(plan) => dispatched_members(&plan, bodies),
        None => Ok(Vec::new()),
    }
}

/// The interface defaults the admitted object `class` inherits, each with the interface
/// declarations it implements for the class. Dispatch reaches one only when a walked body calls
/// one of those declarations, so a closure releases it then.
pub(super) fn object_inherited_defaults(
    class: &crate::libraries::KlibBodyClassContract,
    bodies: &KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
) -> Result<Vec<InheritedDefault>, KlibBodyDeclineReason> {
    if class.kind != TypeKind::Object {
        return Ok(Vec::new());
    }
    let Some(plan) = declared_instances_plan(class, bodies, callees)? else {
        return Ok(Vec::new());
    };
    Ok(inherited_implementations(&plan, bodies)?
        .into_iter()
        .filter(|(body, _)| {
            matches!(
                bodies.member_owner(body),
                crate::klib_libraries::LocatedMember::One { class, .. }
                    if class_kind(class.base.flags) == TypeKind::Interface
            )
        })
        .map(|(body, implemented)| InheritedDefault { body, implemented })
        .collect())
}

/// An interface default a class inherits, with the interface declarations it implements.
#[derive(Clone, Debug)]
pub(crate) struct InheritedDefault {
    pub(crate) body: KlibDependencyCallable,
    pub(crate) implemented: Vec<KlibDependencyCallable>,
}

/// An override of a value class, with the declarations it overrides.
pub(crate) struct ValueClassOverride {
    pub(crate) identity: KlibDependencyCallable,
    pub(crate) overridden: Vec<KlibDependencyCallable>,
}

/// The overrides with bodies of the value class declaring the member `selected`, each with the
/// declarations it overrides: dispatch reaches them on a boxed instance of the class, which has no
/// initialization of its own to carry them.
pub(crate) fn value_class_overrides(
    selected: KlibBodyCallable<'_>,
    bodies: &KlibDeclarationBodies,
) -> Result<Vec<ValueClassOverride>, KlibBodyDeclineReason> {
    let header = class_headers::authenticate(selected, bodies)?;
    if !header.contract.is_value {
        return Ok(Vec::new());
    }
    let module = &header.contract.module;
    let mut overrides = Vec::new();
    for member in &header.class.members {
        let functions: Vec<_> = match member {
            KlibIrMember::Function(function) => vec![*function],
            KlibIrMember::Property(property) => [property.getter, property.setter]
                .into_iter()
                .flatten()
                .collect(),
            _ => Vec::new(),
        };
        for function in functions.into_iter().map(|id| header.arena.function(id)) {
            if function.constructor || function.body.is_none() || function.overridden.is_empty() {
                continue;
            }
            let signature =
                crate::klib_libraries::member_signature(&function.base.symbol.signature)
                    .ok_or_else(|| mismatch("a value class override is not a member"))?;
            let KlibCalleeOrigin::Exact(identity) = bodies.linked_origin(module, &signature) else {
                return Err(mismatch("a value class override has no exact producer"));
            };
            let mut overridden = Vec::new();
            for symbol in &function.overridden {
                let signature = crate::klib_libraries::member_signature(&symbol.signature)
                    .ok_or_else(|| {
                        mismatch("an override names a declaration that is not a member")
                    })?;
                overridden.extend(
                    bodies
                        .overridden_closure(module, &signature)
                        .map_err(mismatch)?,
                );
            }
            overrides.push(ValueClassOverride {
                identity,
                overridden,
            });
        }
    }
    Ok(overrides)
}

fn dispatched_members(
    plan: &ClassInitialization<'_, '_>,
    bodies: &KlibDeclarationBodies,
) -> Result<Vec<KlibDependencyCallable>, KlibBodyDeclineReason> {
    let mut members = plan
        .dispatched
        .iter()
        .filter(|function| function.body.is_some())
        .map(|function| {
            let signature =
                crate::klib_libraries::member_signature(&function.base.symbol.signature)
                    .ok_or_else(|| mismatch("a dispatched member is not a member"))?;
            match bodies.linked_origin(&plan.header.contract.module, &signature) {
                KlibCalleeOrigin::Exact(identity) => Ok(identity),
                KlibCalleeOrigin::Missing | KlibCalleeOrigin::Ambiguous => {
                    Err(mismatch("a dispatched member has no exact producer"))
                }
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    // A member the class inherits implements the interface declarations it reaches only through
    // its supertypes (`Itr.remove` for `MutableIterator.remove` under a subclass), so dispatch
    // through the interface runs the inherited body, which is emitted with the class.
    for (body, _) in inherited_implementations(plan, bodies)? {
        // Only a body a superclass declares: an interface's default is inherited by its own
        // mechanism.
        let from_class = matches!(
            bodies.member_owner(&body),
            crate::klib_libraries::LocatedMember::One { class, .. }
                if class_kind(class.base.flags) == TypeKind::Class
        );
        if from_class && !members.contains(&body) {
            members.push(body);
        }
    }
    Ok(members)
}

/// The bodies an admitted class inherits that implement interface members it reaches only through
/// its supertypes, with the interface declarations each implements.
fn inherited_implementations(
    plan: &ClassInitialization<'_, '_>,
    bodies: &KlibDeclarationBodies,
) -> Result<Vec<(KlibDependencyCallable, Vec<KlibDependencyCallable>)>, KlibBodyDeclineReason> {
    bodies
        .inherited_implementations(
            &plan.header.contract.module,
            plan.header.arena,
            plan.header.class,
        )
        .map_err(mismatch)
}

/// The declarations an admitted class initialization names without emitting their bodies: the
/// ones its overrides override, which their override edges name, and the constructor of a
/// superclass the target lays out, which the target builds. They are declaration evidence only.
pub(super) fn constructor_declaration_evidence(
    selected: KlibBodyCallable<'_>,
    bodies: &KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
) -> Result<Vec<KlibDependencyCallable>, KlibBodyDeclineReason> {
    match plan(selected, bodies, callees)? {
        Some(plan) => overridden_declarations(&plan, bodies),
        None => Ok(Vec::new()),
    }
}

/// The declarations the overrides of the admitted object or enum class `class` override.
pub(super) fn declared_instances_overridden_declarations(
    class: &crate::libraries::KlibBodyClassContract,
    bodies: &KlibDeclarationBodies,
    callees: &super::KlibCalleeFacts<'_>,
) -> Result<Vec<KlibDependencyCallable>, KlibBodyDeclineReason> {
    match declared_instances_plan(class, bodies, callees)? {
        Some(plan) => overridden_declarations(&plan, bodies),
        None => Ok(Vec::new()),
    }
}

fn overridden_declarations(
    plan: &ClassInitialization<'_, '_>,
    bodies: &KlibDeclarationBodies,
) -> Result<Vec<KlibDependencyCallable>, KlibBodyDeclineReason> {
    let mut declarations = plan
        .superclass
        .iter()
        .filter(|superclass| !superclass.materialized)
        .map(|superclass| superclass.constructor.clone())
        .collect::<Vec<_>>();
    for symbol in plan
        .dispatched
        .iter()
        .flat_map(|function| &function.overridden)
    {
        let signature = crate::klib_libraries::member_signature(&symbol.signature)
            .ok_or_else(|| mismatch("an override names a declaration that is not a member"))?;
        declarations.extend(
            bodies
                .overridden_closure(&plan.header.contract.module, &signature)
                .map_err(mismatch)?,
        );
    }
    for (_, implemented) in inherited_implementations(plan, bodies)? {
        declarations.extend(implemented);
    }
    Ok(declarations)
}
