//! JVM realization of a delegated property whose delegate is a property reference.
//!
//! kotlinc's `PropertyReferenceDelegationLowering`: `val x by r::y` needs no `KProperty` instance
//! when its accessors only call the standard library's `getValue`/`setValue`, which read and write
//! the referenced property. Each accessor then calls the referenced getter or setter itself. A
//! bound receiver that may change, or whose evaluation may have side effects, is evaluated once and
//! kept in an `x$receiver` field; a stable one (an immutable `this`, a final default `val` or field
//! of this file over a stable receiver, a constant or a singleton) is evaluated where the delegate
//! was initialized and evaluated again in each accessor. Reflection still reaches the delegate
//! through a `get<X>$delegate` method, which builds the reference reflectively. A local delegated
//! property keeps a bound receiver in its delegate variable, or no variable at all, and has no
//! `$delegate` method.
//!
//! The pass runs before property references are realized, so the reference is never a generated
//! class. It decides from the checked reference, the accessors' checked bodies and the selected
//! conventions' declaration owners; it never consults a spelling. The `get<X>$delegate` methods are
//! installed by [`crate::in_place_delegates`], which owns that method for every delegate kind.

use krusty::fir::{FirCallableReferenceBinding, FirPropertyReferenceTarget, PropertyId};
use krusty::fx_hash::{HashMap, HashSet};
use krusty::ir::{
    Callee, ClassId, ExprId, FunId, IrCheckedOperation, IrConst, IrExpr, IrFile,
    IrLocalPropertyLayout, IrModuleSource, IrTypeOp,
};
use krusty::types::{type_name, SemanticCallableOwner, Ty, TypeName};

/// The reflective reference one generated `get<X>$delegate` method returns, and the line it is
/// stated on.
#[derive(Clone, Copy)]
pub(crate) struct DelegateMethodValue {
    pub(crate) value: ExprId,
    pub(crate) line: u32,
}

/// What the lowering leaves for the passes after it.
#[derive(Default)]
pub(crate) struct PropertyReferenceDelegates {
    /// The references a `get<X>$delegate` method returns. They are realized reflectively, as a
    /// `PropertyReferenceNImpl` naming the property, never as a generated reference class.
    reflected: HashSet<ExprId>,
    package: HashMap<PropertyId, DelegateMethodValue>,
    /// `(declaring class, index into its properties)`.
    member: HashMap<(ClassId, u32), DelegateMethodValue>,
    /// `(declaring class, index into its member extension properties)`.
    member_extension: HashMap<(ClassId, u32), DelegateMethodValue>,
    /// Local delegated-property plans whose accessors no longer take the delegate's storage.
    local_without_storage: HashSet<u32>,
}

impl PropertyReferenceDelegates {
    pub(crate) fn is_reflected(&self, reference: ExprId) -> bool {
        self.reflected.contains(&reference)
    }

    pub(crate) fn package_method(&self, property: PropertyId) -> Option<DelegateMethodValue> {
        self.package.get(&property).copied()
    }

    pub(crate) fn member_method(
        &self,
        class: ClassId,
        property: u32,
    ) -> Option<DelegateMethodValue> {
        self.member.get(&(class, property)).copied()
    }

    /// Whether the accessors of local delegated-property `plan` take no delegate operand.
    pub(crate) fn local_takes_no_storage(&self, plan: u32) -> bool {
        self.local_without_storage.contains(&plan)
    }

    pub(crate) fn member_extension_method(
        &self,
        class: ClassId,
        property: u32,
    ) -> Option<DelegateMethodValue> {
        self.member_extension.get(&(class, property)).copied()
    }
}

/// Where a delegated property keeps its delegate.
#[derive(Clone, Copy)]
enum Storage {
    /// A class field, initialized by the declaration store `store`.
    Field {
        class: ClassId,
        index: u32,
        store: ExprId,
    },
    /// A file-facade static.
    Static(u32),
}

#[derive(Clone, Copy)]
enum Declaration {
    Package(PropertyId),
    Member(ClassId, u32),
    MemberExtension(ClassId, u32),
}

struct Candidate {
    declaration: Declaration,
    name: String,
    getter: FunId,
    setter: Option<FunId>,
    storage: Storage,
    delegate: ExprId,
}

/// The parts of a delegate reference the accessors and the delegate method are built from.
struct Reference {
    target: PropertyId,
    binding: FirCallableReferenceBinding,
    /// The bound receiver, in the slot the reference binds it to.
    bound: Option<(ExprId, ReceiverSlot)>,
    /// The slot the accessor's own receiver takes when the reference is unbound.
    unbound_slot: ReceiverSlot,
    property_type: Ty,
    receiver_type: Option<Ty>,
    operation: IrCheckedOperation,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReceiverSlot {
    Dispatch,
    Extension,
}

/// How each accessor reaches the bound receiver.
#[derive(Clone, Copy)]
enum BoundReceiver {
    /// Evaluated again from the stable expression.
    Inline(ExprId),
    /// Read from the `x$receiver` storage.
    Stored(Storage),
}

/// Lower every member, member-extension and package delegated property whose delegate is a
/// property reference that only the standard library's conventions consume.
pub(crate) fn lower(
    ir: &mut IrFile,
    callables: &krusty::backend::CheckedBackendCallables,
    value_classes: &HashMap<TypeName, Ty>,
    source: IrModuleSource,
) -> PropertyReferenceDelegates {
    let mut lowered = PropertyReferenceDelegates::default();
    let mut removed = HashMap::<ClassId, HashSet<u32>>::default();
    for candidate in candidates(ir) {
        let Some(reference) = optimizable_reference(ir, callables, value_classes, &candidate)
        else {
            continue;
        };
        lower_candidate(ir, source, candidate, reference, &mut lowered, &mut removed);
    }
    crate::field_compaction::remove_fields(ir, &removed);
    lower_locals(ir, callables, value_classes, source, &mut lowered);
    lowered
}

/// A local delegated property whose delegate is an optimizable property reference keeps only its
/// bound receiver, in the delegate's variable; with none, the variable goes. Its accessors read
/// and write the referenced property through that receiver, as kotlinc's
/// `visitLocalDelegatedProperty` does. A local has no reflective `$delegate` method.
fn lower_locals(
    ir: &mut IrFile,
    callables: &krusty::backend::CheckedBackendCallables,
    value_classes: &HashMap<TypeName, Ty>,
    source: IrModuleSource,
    lowered: &mut PropertyReferenceDelegates,
) {
    let candidates = (0..ir.local_delegate_plans.len())
        .filter_map(|plan| local_candidate(ir, callables, value_classes, source, plan as u32))
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return;
    }
    let accesses = local_accesses(ir);
    for (plan, storage, reference) in candidates {
        let Some(accesses) = direct_storage_accesses(ir, plan, storage, &accesses) else {
            continue;
        };
        lower_local(ir, plan, storage, &reference, &accesses, lowered);
    }
}

/// A plan this file declares outside any lambda or inline copy, whose delegate variable holds an
/// optimizable reference consumed only by standard-library conventions.
fn local_candidate(
    ir: &IrFile,
    callables: &krusty::backend::CheckedBackendCallables,
    value_classes: &HashMap<TypeName, Ty>,
    source: IrModuleSource,
    plan: u32,
) -> Option<(u32, ExprId, Reference)> {
    let declaration = &ir.local_delegate_plans[plan as usize];
    if declaration.inline_declaration.is_some()
        || declaration.declaration_lambda.is_some()
        || declaration.reference.source != source
    {
        return None;
    }
    let storage = declaration.storage?;
    let IrExpr::Variable {
        init: Some(delegate),
        ..
    } = ir.expr(storage)
    else {
        return None;
    };
    let reference = delegate_reference(ir, value_classes, *delegate)?;
    // A standard-library convention is no member extension: the storage is the first parameter.
    if declaration.getter.parameters.len() != 1 {
        return None;
    }
    stdlib_call(ir, callables, strip_coercion(ir, declaration.getter.body))?;
    if let Some(setter) = &declaration.setter {
        if setter.parameters.len() != 2 {
            return None;
        }
        stdlib_call(ir, callables, setter.body)?;
    }
    if reference.binding == FirCallableReferenceBinding::Unbound {
        return None;
    }
    Some((plan, storage, reference))
}

/// Every local delegated-property access, by plan.
fn local_accesses(ir: &IrFile) -> HashMap<u32, Vec<ExprId>> {
    let mut accesses = HashMap::<u32, Vec<ExprId>>::default();
    for (expression, node) in ir.exprs.iter().enumerate() {
        if let IrExpr::LocalDelegateAccess(access) = node {
            accesses
                .entry(access.plan)
                .or_default()
                .push(expression as ExprId);
        }
    }
    accesses
}

/// The accesses of `plan`, when each reads the delegate variable `storage` directly in the body
/// declaring it and nothing else in that body reads the variable's slot.
fn direct_storage_accesses(
    ir: &IrFile,
    plan: u32,
    storage: ExprId,
    accesses: &HashMap<u32, Vec<ExprId>>,
) -> Option<Vec<ExprId>> {
    let IrExpr::Variable { index, .. } = ir.expr(storage) else {
        return None;
    };
    let tree = declaring_tree(ir, storage)?;
    let accesses = accesses.get(&plan).cloned().unwrap_or_default();
    let mut delegates = HashSet::default();
    for &access in &accesses {
        let IrExpr::LocalDelegateAccess(access_node) = ir.expr(access) else {
            return None;
        };
        if !tree.contains(&access)
            || !matches!(ir.expr(access_node.delegate), IrExpr::GetValue(slot) if slot == index)
        {
            return None;
        }
        delegates.insert(access_node.delegate);
    }
    let other_read = tree.iter().any(|&expression| {
        matches!(ir.expr(expression), IrExpr::GetValue(slot) if slot == index)
            && !delegates.contains(&expression)
    });
    (!other_read).then_some(accesses)
}

/// The expressions of the emitted body that contains `expression`.
fn declaring_tree(ir: &IrFile, expression: ExprId) -> Option<HashSet<ExprId>> {
    let roots = ir
        .functions
        .iter()
        .filter_map(|function| function.body)
        .chain(ir.classes.iter().filter_map(|class| class.init_body));
    for root in roots {
        let mut tree = HashSet::default();
        let mut pending = vec![root];
        while let Some(node) = pending.pop() {
            if tree.insert(node) {
                krusty::ir::for_each_child(&ir.exprs, node, &mut |child| pending.push(child));
            }
        }
        if tree.contains(&expression) {
            return Some(tree);
        }
    }
    None
}

fn lower_local(
    ir: &mut IrFile,
    plan: u32,
    storage: ExprId,
    reference: &Reference,
    accesses: &[ExprId],
    lowered: &mut PropertyReferenceDelegates,
) {
    let IrExpr::Variable {
        init: Some(delegate),
        ..
    } = *ir.expr(storage)
    else {
        unreachable!("a local candidate's storage is an initialized variable");
    };
    let bound = reference.bound.map(|(bound, _)| bound);
    let receiver_type = bound.map(|bound| {
        ir.logical_type(bound)
            .or(reference.receiver_type)
            .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")))
    });
    let declaration = &ir.local_delegate_plans[plan as usize];
    let getter = declaration.getter.body;
    let getter_result = declaration.getter.result;
    let setter = declaration.setter.as_ref().map(|setter| setter.body);

    // The accessors read the receiver from their first parameter.
    let first_built = ir.exprs.len();
    let receiver = bound.map(|_| ir.add_expr(IrExpr::GetValue(0)));
    let getter_body = accessor_value(ir, reference, receiver, None, None, getter_result);
    take_position(ir, first_built, Some(getter));
    let setter_body = setter.map(|setter| {
        let first_built = ir.exprs.len();
        let receiver = bound.map(|_| ir.add_expr(IrExpr::GetValue(0)));
        let value = ir.add_expr(IrExpr::GetValue(u32::from(bound.is_some())));
        ir.set_logical_type(value, reference.property_type);
        let body = accessor_value(ir, reference, receiver, None, Some(value), Ty::Unit);
        take_position(ir, first_built, Some(setter));
        body
    });

    let declaration = &mut ir.local_delegate_plans[plan as usize];
    declaration.getter.body = getter_body;
    let accessors = std::iter::once(&mut declaration.getter).chain(declaration.setter.as_mut());
    for accessor in accessors {
        match receiver_type {
            Some(ty) => accessor.parameters[0] = ty,
            None => {
                accessor.parameters.remove(0);
                accessor.parameter_identities.remove(0);
            }
        }
    }
    if let (Some(setter), Some(body)) = (declaration.setter.as_mut(), setter_body) {
        setter.body = body;
    }

    // The variable keeps the receiver, or goes; its reads were the accesses' delegate operands.
    match (bound, receiver_type) {
        (Some(bound), Some(ty)) => {
            if let IrExpr::Variable {
                ty: stored, init, ..
            } = &mut ir.exprs[storage as usize]
            {
                *stored = ty;
                *init = Some(bound);
            }
        }
        _ => {
            ir.exprs[storage as usize] = IrExpr::Block {
                stmts: Vec::new(),
                value: None,
            };
            lowered.local_without_storage.insert(plan);
            for &access in accesses {
                if let IrExpr::LocalDelegateAccess(access) = ir.expr(access) {
                    let read = access.delegate;
                    ir.exprs[read as usize] = IrExpr::Const(IrConst::Null);
                }
            }
        }
    }
    ir.exprs[delegate as usize] = IrExpr::Const(IrConst::Null);
    retire_expressions(
        ir,
        &std::iter::once(getter).chain(setter).collect::<Vec<_>>(),
    );
}

fn candidates(ir: &IrFile) -> Vec<Candidate> {
    let mut candidates = ir
        .package_properties
        .iter()
        .filter(|declaration| declaration.context_parameters.is_empty())
        .filter_map(
            |declaration| match ir.local_property_layouts.get(&declaration.property) {
                Some(IrLocalPropertyLayout::TopLevelAccessor {
                    getter,
                    setter,
                    delegate: Some(storage),
                    ..
                }) => Some(Candidate {
                    declaration: Declaration::Package(declaration.property),
                    name: declaration.name.clone(),
                    getter: *getter,
                    setter: *setter,
                    storage: Storage::Static(*storage),
                    delegate: ir.statics[*storage as usize].init?,
                }),
                _ => None,
            },
        )
        .collect::<Vec<_>>();
    for class in 0..ir.classes.len() {
        let class = class as ClassId;
        for (property, declaration) in ir.classes[class as usize].properties.iter().enumerate() {
            if !declaration.context_params.is_empty() {
                continue;
            }
            let (Some(field), Some(getter)) = (declaration.delegate_field, declaration.getter)
            else {
                continue;
            };
            let Some((store, delegate)) = delegate_store(ir, class, field) else {
                continue;
            };
            candidates.push(Candidate {
                declaration: Declaration::Member(class, property as u32),
                name: declaration.name.clone(),
                getter,
                setter: declaration.setter,
                storage: Storage::Field {
                    class,
                    index: field,
                    store,
                },
                delegate,
            });
        }
        let owner = ir.classes[class as usize].fq_name;
        for (property, declaration) in ir
            .member_ext_props
            .get(&owner)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let Some(field) = declaration.delegate_field else {
                continue;
            };
            let Some((store, delegate)) = delegate_store(ir, class, field) else {
                continue;
            };
            candidates.push(Candidate {
                declaration: Declaration::MemberExtension(class, property as u32),
                name: declaration.name.clone(),
                getter: declaration.getter,
                setter: declaration.setter,
                storage: Storage::Field {
                    class,
                    index: field,
                    store,
                },
                delegate,
            });
        }
    }
    candidates
}

/// The declaration store that initializes `class`'s delegate `field`, and the delegate it stores.
fn delegate_store(ir: &IrFile, class: ClassId, field: u32) -> Option<(ExprId, ExprId)> {
    ir.property_initializer_stores
        .iter()
        .copied()
        .find_map(|store| match ir.expr(store) {
            IrExpr::SetField {
                class: stored_class,
                index,
                value,
                ..
            } if *stored_class == class && *index == field => Some((store, *value)),
            _ => None,
        })
}

/// kotlinc's `getRichPropertyReferenceForOptimizableDelegatedProperty`: the delegate is a property
/// reference and each accessor returns the result of one call to a standard-library declaration.
fn optimizable_reference(
    ir: &IrFile,
    callables: &krusty::backend::CheckedBackendCallables,
    value_classes: &HashMap<TypeName, Ty>,
    candidate: &Candidate,
) -> Option<Reference> {
    let reference = delegate_reference(ir, value_classes, candidate.delegate)?;
    stdlib_convention(ir, callables, candidate.getter, false)?;
    if let Some(setter) = candidate.setter {
        stdlib_convention(ir, callables, setter, true)?;
    }
    Some(reference)
}

/// The delegate `delegate`, when it is a reference to a property of this module that the
/// accessors can read and write directly.
fn delegate_reference(
    ir: &IrFile,
    value_classes: &HashMap<TypeName, Ty>,
    delegate: ExprId,
) -> Option<Reference> {
    let IrExpr::Checked(operation) = ir.expr(delegate) else {
        return None;
    };
    let IrCheckedOperation::PropertyReference {
        target:
            FirPropertyReferenceTarget::SpecializedModule {
                property,
                extension_receiver,
                property_type,
                receiver,
                ..
            },
        delegated: false,
        binding,
        dispatch_receiver,
        extension_receiver: bound_extension,
        context_arguments,
        adaptation: None,
        ..
    } = &**operation
    else {
        return None;
    };
    if !context_arguments.is_empty() {
        return None;
    }
    if !has_final_getter_signature(ir, value_classes, *property) {
        return None;
    }
    let bound = match (binding, dispatch_receiver, bound_extension) {
        (FirCallableReferenceBinding::Bound, Some(receiver), None) => {
            Some((*receiver, ReceiverSlot::Dispatch))
        }
        (FirCallableReferenceBinding::Bound, None, Some(receiver)) => {
            Some((*receiver, ReceiverSlot::Extension))
        }
        (
            FirCallableReferenceBinding::Unbound | FirCallableReferenceBinding::Static,
            None,
            None,
        ) => None,
        _ => return None,
    };
    Some(Reference {
        target: *property,
        binding: *binding,
        bound,
        unbound_slot: if *extension_receiver {
            ReceiverSlot::Extension
        } else {
            ReceiverSlot::Dispatch
        },
        property_type: property_type.get(),
        receiver_type: receiver.map(krusty::fir::ResolvedTy::get),
        operation: (**operation).clone(),
    })
}

/// Whether the delegate method can name the referenced getter by its final JVM signature. An
/// accessor this file declares publishes it once every naming pass has run. A default accessor is
/// named `get<Name>` over its declared types, with an internal member's module suffix, unless
/// value-class mangling renames it.
fn has_final_getter_signature(
    ir: &IrFile,
    value_classes: &HashMap<TypeName, Ty>,
    property: PropertyId,
) -> bool {
    if declared_getter(ir, property).is_some() {
        return true;
    }
    let Some(declaration) = ir.referenced_module_properties.get(&property) else {
        return false;
    };
    let names_value_class = |ty: Ty| match ty.non_null().kind() {
        krusty::types::TyKind::Obj(classifier, _) => {
            value_classes.contains_key(&classifier) || ir.is_value_class_name(classifier)
        }
        _ => false,
    };
    !names_value_class(declaration.ty)
        && !declaration
            .extension_receiver
            .is_some_and(names_value_class)
        && !declaration.owner.is_some_and(|owner| {
            value_classes.contains_key(&owner) || ir.is_value_class_name(owner)
        })
}

/// The getter this file declares for `property`.
pub(crate) fn declared_getter(ir: &IrFile, property: PropertyId) -> Option<FunId> {
    match ir.local_property_layouts.get(&property)? {
        IrLocalPropertyLayout::TopLevelStorage { getter, .. }
        | IrLocalPropertyLayout::Member { getter, .. } => *getter,
        IrLocalPropertyLayout::TopLevelAccessor { getter, .. }
        | IrLocalPropertyLayout::MemberExtension { getter, .. } => Some(*getter),
    }
}

/// The convention call an accessor returns, when it is a call of a standard-library declaration:
/// kotlinc's `returnsResultOfStdlibCall`. Returns the call's value arguments.
fn stdlib_convention(
    ir: &IrFile,
    callables: &krusty::backend::CheckedBackendCallables,
    accessor: FunId,
    setter: bool,
) -> Option<Vec<ExprId>> {
    let body = ir.functions[accessor as usize].body?;
    let IrExpr::Block { stmts, value: None } = ir.expr(body) else {
        return None;
    };
    let call = match (stmts.as_slice(), setter) {
        ([returned], false) => match ir.expr(*returned) {
            IrExpr::Return(Some(call)) => strip_coercion(ir, *call),
            _ => return None,
        },
        ([call], true) => *call,
        ([call, returned], true) if matches!(ir.expr(*returned), IrExpr::Return(None)) => *call,
        _ => return None,
    };
    stdlib_call(ir, callables, call)
}

/// The value arguments of `call`, when it calls a standard-library declaration.
fn stdlib_call(
    ir: &IrFile,
    callables: &krusty::backend::CheckedBackendCallables,
    call: ExprId,
) -> Option<Vec<ExprId>> {
    let IrExpr::Call { callee, args, .. } = ir.expr(call) else {
        return None;
    };
    let Callee::External { target, .. } = &**callee else {
        return None;
    };
    let kotlin = type_name("kotlin");
    match callables.callable(*target)?.declaration_owner? {
        SemanticCallableOwner::Package(package) if package == kotlin => {}
        SemanticCallableOwner::Classifier(classifier) if classifier.namespace() == kotlin => {}
        SemanticCallableOwner::Package(_) | SemanticCallableOwner::Classifier(_) => return None,
    }
    Some(args.clone())
}

fn lower_candidate(
    ir: &mut IrFile,
    source: IrModuleSource,
    candidate: Candidate,
    reference: Reference,
    lowered: &mut PropertyReferenceDelegates,
    removed: &mut HashMap<ClassId, HashSet<u32>>,
) {
    let getter_arguments = arguments(ir, candidate.getter);
    let setter_arguments = candidate.setter.map(|setter| arguments(ir, setter));
    let line = match candidate.storage {
        Storage::Field { class, index, .. } => {
            ir.classes[class as usize].fields[index as usize].constructor_store_line
        }
        Storage::Static(_) => accessor_line(ir, candidate.getter),
    };
    let receiver = reference.bound.map(|(bound, _)| {
        if is_stable(ir, source, bound, candidate.storage) {
            BoundReceiver::Inline(bound)
        } else {
            BoundReceiver::Stored(candidate.storage)
        }
    });

    // The accessors call the referenced accessors directly.
    let old_getter = ir.functions[candidate.getter as usize].body;
    let getter_call = old_getter.map(|body| convention_statement(ir, body));
    let getter_receiver = getter_arguments.first().copied();
    let first_built = ir.exprs.len();
    let getter_body = accessor_body(
        ir,
        &reference,
        receiver,
        getter_receiver,
        None,
        candidate.getter,
    );
    take_position(ir, first_built, getter_call);
    ir.functions[candidate.getter as usize].body = Some(getter_body);
    let mut retired = old_getter.into_iter().collect::<Vec<_>>();
    if let (Some(setter), Some(arguments)) = (candidate.setter, setter_arguments) {
        let old_setter = ir.functions[setter as usize].body;
        let setter_call = old_setter.map(|body| convention_statement(ir, body));
        let value = arguments.get(2).copied();
        let first_built = ir.exprs.len();
        let body = accessor_body(
            ir,
            &reference,
            receiver,
            arguments.first().copied(),
            value,
            setter,
        );
        take_position(ir, first_built, setter_call);
        ir.functions[setter as usize].body = Some(body);
        retired.extend(old_setter);
    }

    // The delegate method returns the reference, built reflectively over the same receiver.
    let method_receiver = receiver.map(|receiver| receiver_read(ir, receiver));
    let mut operation = reference.operation.clone();
    if let IrCheckedOperation::PropertyReference {
        dispatch_receiver,
        extension_receiver,
        ..
    } = &mut operation
    {
        match reference.bound.map(|(_, slot)| slot) {
            Some(ReceiverSlot::Dispatch) => *dispatch_receiver = method_receiver,
            Some(ReceiverSlot::Extension) => *extension_receiver = method_receiver,
            None => {}
        }
    }
    let value = ir.add_expr(IrExpr::Checked(Box::new(operation)));
    ir.copy_types(candidate.delegate, value);
    if ir.source_line(candidate.delegate).is_some() {
        ir.copy_position(candidate.delegate, value);
    }
    lowered.reflected.insert(value);
    let method = DelegateMethodValue { value, line };
    match candidate.declaration {
        Declaration::Package(property) => {
            lowered.package.insert(property, method);
        }
        Declaration::Member(class, property) => {
            lowered.member.insert((class, property), method);
        }
        Declaration::MemberExtension(class, property) => {
            lowered.member_extension.insert((class, property), method);
        }
    }

    // The delegate's storage now holds the receiver, or nothing.
    let bound = reference.bound.map(|(bound, _)| bound);
    match receiver {
        Some(BoundReceiver::Stored(storage)) => {
            let bound = bound.expect("a stored receiver is bound");
            let ty = ir
                .logical_type(bound)
                .or(reference.receiver_type)
                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
            let name = format!("{}$receiver", candidate.name);
            match storage {
                Storage::Field {
                    class,
                    index,
                    store,
                } => {
                    let field = &mut ir.classes[class as usize].fields[index as usize];
                    field.name = name;
                    field.ty = ty;
                    if let IrExpr::SetField { value, .. } = &mut ir.exprs[store as usize] {
                        *value = bound;
                    }
                }
                Storage::Static(index) => {
                    let stored = &mut ir.statics[index as usize];
                    stored.name = name;
                    stored.ty = ty;
                    stored.init = Some(bound);
                }
            }
        }
        Some(BoundReceiver::Inline(_)) | None => {
            retire_storage(ir, &candidate, bound, removed);
        }
    }
    // The reference node itself is now unreachable; its receiver lives on where it moved.
    ir.exprs[candidate.delegate as usize] = IrExpr::Const(IrConst::Null);
    retire_expressions(ir, &retired);
}

/// The statement of an optimizable accessor's body: the convention call's `return`, or the call.
fn convention_statement(ir: &IrFile, body: ExprId) -> ExprId {
    match ir.expr(body) {
        IrExpr::Block { stmts, .. } => stmts[0],
        _ => unreachable!("an optimizable accessor body is a block"),
    }
}

/// The nodes a new accessor body built from `first_built` on stand where the convention call
/// stood, as kotlinc builds the replacement at the accessor's offsets. A copied receiver keeps the
/// position it was copied with.
fn take_position(ir: &mut IrFile, first_built: usize, statement: Option<ExprId>) {
    let Some(statement) = statement.filter(|&statement| ir.source_line(statement).is_some()) else {
        return;
    };
    for expression in first_built..ir.exprs.len() {
        let expression = expression as ExprId;
        if ir.source_line(expression).is_none() {
            ir.copy_position(statement, expression);
        }
    }
}

/// The value arguments of the convention call an accessor returns.
fn arguments(ir: &IrFile, accessor: FunId) -> Vec<ExprId> {
    let body = ir.functions[accessor as usize]
        .body
        .expect("an optimizable accessor has a body");
    let IrExpr::Block { stmts, .. } = ir.expr(body) else {
        unreachable!("an optimizable accessor body is a block");
    };
    let call = match ir.expr(stmts[0]) {
        IrExpr::Return(Some(call)) => strip_coercion(ir, *call),
        _ => stmts[0],
    };
    match ir.expr(call) {
        IrExpr::Call { args, .. } => args.clone(),
        _ => unreachable!("an optimizable accessor returns a convention call"),
    }
}

/// `return r.y` or `r.y = value`, where `r` is the accessor's view of the reference's receiver.
fn accessor_body(
    ir: &mut IrFile,
    reference: &Reference,
    bound: Option<BoundReceiver>,
    this_reference: Option<ExprId>,
    value: Option<ExprId>,
    accessor: FunId,
) -> ExprId {
    let bound = bound.map(|bound| receiver_read(ir, bound));
    let result = ir.functions[accessor as usize].ret;
    let access = accessor_value(ir, reference, bound, this_reference, value, result);
    let statements = if value.is_some() {
        vec![access, ir.add_expr(IrExpr::Return(None))]
    } else {
        vec![ir.add_expr(IrExpr::Return(Some(access)))]
    };
    ir.add_expr(IrExpr::Block {
        stmts: statements,
        value: None,
    })
}

/// `r.y` as `result`, or `r.y = value`, where `r` is the read `bound` of a bound reference's
/// receiver or a copy of the accessor's `this_reference` for an unbound one.
fn accessor_value(
    ir: &mut IrFile,
    reference: &Reference,
    bound: Option<ExprId>,
    this_reference: Option<ExprId>,
    value: Option<ExprId>,
    result: Ty,
) -> ExprId {
    let receiver = match (reference.binding, bound) {
        (FirCallableReferenceBinding::Bound, Some(bound)) => Some((
            bound,
            reference
                .bound
                .map(|(_, slot)| slot)
                .expect("a bound reference"),
        )),
        (FirCallableReferenceBinding::Unbound, _) => this_reference
            .map(|this_reference| (copy_operand(ir, this_reference), reference.unbound_slot)),
        _ => None,
    };
    let (dispatch_receiver, extension_receiver) = match receiver {
        Some((receiver, ReceiverSlot::Dispatch)) => (Some(receiver), None),
        Some((receiver, ReceiverSlot::Extension)) => (None, Some(receiver)),
        None => (None, None),
    };
    let substitutions = match &reference.operation {
        IrCheckedOperation::PropertyReference { substitutions, .. } => substitutions.clone(),
        _ => unreachable!("an optimizable delegate is a property reference"),
    };
    let constant = ir
        .referenced_module_properties
        .get(&reference.target)
        .and_then(|declaration| declaration.compile_time_constant.clone());
    match value {
        // A constant is its value, whatever reads it (KT-63580).
        None if let Some(constant) = constant => {
            let read = ir.add_expr(IrExpr::Const(constant));
            ir.set_logical_type(read, reference.property_type);
            // The value keeps the constant's own position, as a copy of its initializer.
            if let Some(line) = ir
                .checked_properties
                .get(&reference.target)
                .map(|property| property.decl_line)
                .filter(|&line| line != 0)
            {
                ir.set_source_line(read, line);
            }
            coerce(ir, read, reference.property_type, result)
        }
        None => {
            let read = ir.add_expr(IrExpr::Checked(Box::new(
                IrCheckedOperation::PropertyRead {
                    target: reference.target,
                    dispatch_receiver,
                    extension_receiver,
                    context_arguments: Vec::new(),
                    substitutions,
                },
            )));
            ir.set_logical_type(read, reference.property_type);
            coerce(ir, read, reference.property_type, result)
        }
        Some(value) => {
            let value = copy_operand(ir, value);
            let value = coerce(
                ir,
                value,
                ir.logical_type(value).unwrap_or(reference.property_type),
                reference.property_type,
            );
            ir.add_expr(IrExpr::Checked(Box::new(
                IrCheckedOperation::PropertyWrite {
                    target: reference.target,
                    dispatch_receiver,
                    extension_receiver,
                    context_arguments: Vec::new(),
                    value,
                    substitutions,
                },
            )))
        }
    }
}

/// `expression` without an implicit coercion to the type its consumer expects.
fn strip_coercion(ir: &IrFile, expression: ExprId) -> ExprId {
    match ir.expr(expression) {
        IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg,
            ..
        } => *arg,
        _ => expression,
    }
}

/// A fresh copy of a convention operand, without the implicit coercion to the convention's
/// parameter type.
fn copy_operand(ir: &mut IrFile, operand: ExprId) -> ExprId {
    let operand = strip_coercion(ir, operand);
    krusty::ir::clone_expression_dag(ir, operand).0
}

fn coerce(ir: &mut IrFile, value: ExprId, from: Ty, to: Ty) -> ExprId {
    if from == to {
        return value;
    }
    ir.add_expr(IrExpr::type_op(IrTypeOp::ImplicitCoercion, value, to))
}

/// A fresh read of the bound receiver.
fn receiver_read(ir: &mut IrFile, receiver: BoundReceiver) -> ExprId {
    match receiver {
        BoundReceiver::Inline(bound) => krusty::ir::clone_expression_dag(ir, bound).0,
        BoundReceiver::Stored(Storage::Field { class, index, .. }) => {
            let this = ir.add_expr(IrExpr::GetValue(0));
            ir.add_expr(IrExpr::GetField {
                receiver: this,
                class,
                index,
            })
        }
        BoundReceiver::Stored(Storage::Static(index)) => ir.add_expr(IrExpr::GetStatic(index)),
    }
}

/// kotlinc's `canInline`: whether evaluating the bound receiver again in each accessor yields the
/// same value, with no effect beyond the first evaluation.
fn is_stable(ir: &IrFile, source: IrModuleSource, expression: ExprId, storage: Storage) -> bool {
    match ir.expr(expression) {
        // Within a class's initialization only its own `this` is an immutable value in the
        // accessors' scope; a constructor parameter is not.
        IrExpr::GetValue(0) => matches!(storage, Storage::Field { .. }),
        IrExpr::GetValue(_) => false,
        IrExpr::GetField {
            receiver,
            class,
            index,
        } => {
            ir.classes[*class as usize].fields[*index as usize].is_final()
                && is_stable(ir, source, *receiver, storage)
        }
        IrExpr::GetStatic(index) => {
            let stored = &ir.statics[*index as usize];
            !stored.is_var && !stored.is_lateinit
        }
        IrExpr::Checked(operation) => match &**operation {
            IrCheckedOperation::PropertyRead {
                target,
                dispatch_receiver,
                extension_receiver,
                context_arguments,
                ..
            } => {
                is_final_default_val(ir, source, *target)
                    && context_arguments.is_empty()
                    && dispatch_receiver
                        .is_none_or(|receiver| is_stable(ir, source, receiver, storage))
                    && extension_receiver
                        .is_none_or(|receiver| is_stable(ir, source, receiver, storage))
            }
            _ => false,
        },
        // Constants and singleton accesses are trivially stable.
        IrExpr::Const(
            IrConst::UByte(_) | IrConst::UShort(_) | IrConst::UInt(_) | IrConst::ULong(_),
        ) => false,
        IrExpr::Const(_)
        | IrExpr::SingletonValue { .. }
        | IrExpr::UnitInstance
        | IrExpr::StaticInstance { .. }
        | IrExpr::ExternalStaticInstance { .. } => true,
        _ => false,
    }
}

/// A final `val` of this file with the default getter: a property a `var` override cannot replace.
fn is_final_default_val(ir: &IrFile, source: IrModuleSource, property: PropertyId) -> bool {
    use krusty::fir::DeclarationFlags;
    ir.referenced_module_properties
        .get(&property)
        .is_some_and(|declaration| {
            declaration.source == source
                && !declaration.mutable
                && is_final(declaration)
                && !declaration.flags.has(DeclarationFlags::CUSTOM_GETTER)
                && !declaration.flags.has(DeclarationFlags::DELEGATED)
                && !declaration.flags.has(DeclarationFlags::EXPECT)
                && !declaration.flags.has(DeclarationFlags::EXTERNAL)
        })
}

/// A declaration's modality is final: written `final`, or neither open, abstract nor an override,
/// outside an interface.
fn is_final(declaration: &krusty::ir::IrModuleProperty) -> bool {
    use krusty::fir::DeclarationFlags;
    let flags = declaration.flags;
    flags.has(DeclarationFlags::FINAL)
        || (!flags.has(DeclarationFlags::OPEN)
            && !flags.has(DeclarationFlags::ABSTRACT)
            && !flags.has(DeclarationFlags::OVERRIDE)
            && declaration.owner_kind != Some(krusty::ir::IrClassifierKind::Interface))
}

/// The delegate needs no storage: its declaration store becomes the bound receiver's evaluation,
/// which may initialize a class, or disappears.
fn retire_storage(
    ir: &mut IrFile,
    candidate: &Candidate,
    bound: Option<ExprId>,
    removed: &mut HashMap<ClassId, HashSet<u32>>,
) {
    match candidate.storage {
        Storage::Field {
            class,
            index,
            store,
        } => {
            ir.property_initializer_stores.remove(&store);
            match bound {
                Some(bound) => {
                    let line =
                        ir.classes[class as usize].fields[index as usize].constructor_store_line;
                    // Move the receiver into the store's place, so no copy of it is left behind.
                    ir.exprs[store as usize] = std::mem::replace(
                        &mut ir.exprs[bound as usize],
                        IrExpr::Const(IrConst::Null),
                    );
                    ir.copy_types(bound, store);
                    ir.record_jvm_delegate_evaluation(store);
                    if line != 0 {
                        ir.set_source_line(store, line);
                    }
                }
                None => {
                    for expression in &mut ir.exprs {
                        if let IrExpr::Block { stmts, .. } = expression {
                            stmts.retain(|&statement| statement != store);
                        }
                    }
                    ir.exprs[store as usize] = IrExpr::Const(IrConst::Null);
                }
            }
            match candidate.declaration {
                Declaration::Member(class, property) => {
                    ir.classes[class as usize].properties[property as usize].delegate_field = None;
                }
                Declaration::MemberExtension(class, property) => {
                    let owner = ir.classes[class as usize].fq_name;
                    ir.member_ext_props
                        .get_mut(&owner)
                        .expect("a collected member extension keeps its owner")
                        [property as usize]
                        .delegate_field = None;
                }
                Declaration::Package(_) => unreachable!("a package delegate is a static"),
            }
            removed.entry(class).or_default().insert(index);
        }
        Storage::Static(index) => {
            // A singleton is still evaluated where the delegate was initialized.
            ir.statics[index as usize].init = bound;
            ir.retire_jvm_delegate_static(index);
            let Declaration::Package(property) = candidate.declaration else {
                unreachable!("a static delegate belongs to a package property");
            };
            if let Some(IrLocalPropertyLayout::TopLevelAccessor { delegate, .. }) =
                ir.local_property_layouts.get_mut(&property)
            {
                *delegate = None;
            }
        }
    }
}

/// Make the replaced accessor bodies and delegate inert: every node stays in the arena, and later
/// passes walk the whole arena, so none of them may still read the retired storage or realize the
/// retired references.
fn retire_expressions(ir: &mut IrFile, roots: &[ExprId]) {
    let mut pending = roots.to_vec();
    let mut seen = HashSet::default();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        krusty::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
        ir.exprs[expression as usize] = IrExpr::Const(IrConst::Null);
    }
}

/// The source line a generated getter's value is stated on: its property's declaration line.
fn accessor_line(ir: &IrFile, getter: FunId) -> u32 {
    let Some(body) = ir.functions[getter as usize].body else {
        return 0;
    };
    match ir.expr(body) {
        IrExpr::Block { stmts, .. } => stmts
            .iter()
            .find_map(|&statement| ir.source_line(statement))
            .unwrap_or(0),
        _ => 0,
    }
}
