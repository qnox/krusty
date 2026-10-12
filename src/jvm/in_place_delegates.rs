//! JVM realization of a delegated property whose delegate is a singleton or a constant.
//!
//! kotlinc's `SingletonOrConstantDelegationLowering`: such a delegate needs no storage, because
//! evaluating it again yields the same value. The JVM therefore declares no `x$delegate` field.
//! Each accessor evaluates the delegate where it read the field, and a private static
//! `get<X>$delegate` method returns it for reflection's `getDelegate`, named from the property's
//! `JvmPropertySignature`. A singleton is still evaluated where the field was initialized, since
//! reading it may initialize its class; a constant has nothing to evaluate.
//!
//! The decision reads the checked initializer's IR shape and the exact storage identity common
//! lowering recorded for the property; it never consults a spelling. Runs before companion
//! hoisting, which then finds no delegate field to move. The method is recorded with its getter, so
//! value-class lowering renames it from the getter's mangled name (`getX-<hash>$delegate`).

use crate::fx_hash::{HashMap, HashSet};
use crate::ir::{
    ClassId, ExprId, FnParamInfo, FunId, IrConst, IrExpr, IrFile, IrFunction,
    IrGeneratedParameterRole, IrLocalPropertyLayout, IrParameterIdentity, IrTypeOp,
};
use crate::types::{Ty, Visibility};

/// What evaluating a delegate in place keeps at its initialization site.
#[derive(Clone, Copy, PartialEq, Eq)]
enum InPlaceDelegate {
    /// An object, companion or enum entry: reading it may run its class initializer.
    Singleton,
    /// A literal: nothing to evaluate.
    Constant,
}

fn in_place_delegate(ir: &IrFile, delegate: ExprId) -> Option<InPlaceDelegate> {
    match ir.expr(delegate) {
        IrExpr::SingletonValue { .. }
        | IrExpr::UnitInstance
        | IrExpr::EnumEntry { .. }
        | IrExpr::StaticInstance { .. }
        | IrExpr::ExternalStaticInstance { .. } => Some(InPlaceDelegate::Singleton),
        // An unsigned literal is a value-class construction on the JVM, not a constant there.
        IrExpr::Const(
            IrConst::UByte(_) | IrConst::UShort(_) | IrConst::UInt(_) | IrConst::ULong(_),
        ) => None,
        IrExpr::Const(_) => Some(InPlaceDelegate::Constant),
        _ => None,
    }
}

/// Realize every package and member delegated property whose delegate is a singleton or constant.
pub fn realize(ir: &mut IrFile) {
    realize_package_delegates(ir);
    realize_member_delegates(ir);
}

/// Extend each generated delegate method from its getter's final internal JVM name.
///
/// Value-class lowering may already have added both the getter's signature hash and the delegate
/// method's own hash. Internal naming runs later and records the getter's exact old identity here;
/// replacing only that prefix preserves the delegate suffix without deriving a property name.
pub(crate) fn follow_internal_getter_names(ir: &mut IrFile, old_names: &HashMap<FunId, String>) {
    let renames = ir
        .jvm_delegate_methods_with_getters()
        .filter_map(|(method, getter)| {
            let old_getter = old_names.get(&getter)?;
            let method_name = &ir.functions[method as usize].name;
            let suffix = method_name
                .strip_prefix(old_getter)
                .filter(|suffix| suffix.starts_with("$delegate"))
                .expect("an in-place delegate method extends its recorded getter's JVM name");
            Some((
                method,
                format!("{}{suffix}", ir.functions[getter as usize].name),
            ))
        })
        .collect::<Vec<_>>();
    for (method, name) in renames {
        ir.functions[method as usize].name = name;
    }
}

fn realize_package_delegates(ir: &mut IrFile) {
    let candidates = ir
        .package_properties
        .iter()
        .filter(|declaration| declaration.context_parameters.is_empty())
        .filter_map(
            |declaration| match ir.local_property_layouts.get(&declaration.property) {
                Some(IrLocalPropertyLayout::TopLevelAccessor {
                    getter,
                    delegate: Some(storage),
                    receiver,
                    ..
                }) => Some((
                    declaration.property,
                    *getter,
                    *storage,
                    receiver.map(|receiver| {
                        approximate_delegate_receiver(
                            receiver,
                            declaration
                                .type_params
                                .iter()
                                .map(|parameter| parameter.semantic_name.as_str()),
                            std::iter::empty(),
                        )
                    }),
                )),
                _ => None,
            },
        )
        .collect::<Vec<_>>();
    for (property, getter, storage, receiver) in candidates {
        let Some(delegate) = ir.statics[storage as usize].init else {
            continue;
        };
        let Some(kind) = in_place_delegate(ir, delegate) else {
            continue;
        };
        let storage_ty = ir.statics[storage as usize].ty;
        replace_operations(
            ir,
            |expression| matches!(expression, IrExpr::GetStatic(index) if *index == storage),
            delegate,
        );
        if kind == InPlaceDelegate::Constant {
            ir.statics[storage as usize].init = None;
        }
        ir.retire_jvm_delegate_static(storage);
        if let Some(IrLocalPropertyLayout::TopLevelAccessor { delegate, .. }) =
            ir.local_property_layouts.get_mut(&property)
        {
            *delegate = None;
        }
        let line = accessor_line(ir, getter);
        let method = add_delegate_method(
            ir,
            DelegateMethod {
                name: format!("{}$delegate", ir.functions[getter as usize].name),
                receivers: receiver.into_iter().collect(),
                delegate,
                storage_ty,
                line,
                source_order_of: getter,
            },
        );
        ir.record_jvm_package_delegate_method(property, method, getter);
    }
}

fn realize_member_delegates(ir: &mut IrFile) {
    let mut removed = HashMap::<ClassId, HashSet<u32>>::default();
    let annotation_markers = ir
        .property_annotation_markers
        .values()
        .copied()
        .collect::<HashSet<_>>();
    for class in 0..ir.classes.len() {
        let class = class as ClassId;
        let owner = ir.classes[class as usize].fq_name;
        let owner_receiver = approximate_owner_receiver(ir, owner);
        let owner_type_parameters = ir
            .class_signature_name(owner)
            .into_iter()
            .flat_map(|signature| &signature.type_params)
            .map(|parameter| parameter.semantic_name.clone())
            .chain(
                ir.classes[class as usize]
                    .captured_type_params
                    .iter()
                    .cloned(),
            )
            .collect::<Vec<_>>();
        let mut candidates = ir.classes[class as usize]
            .properties
            .iter()
            .enumerate()
            .filter(|(_, declaration)| declaration.context_params.is_empty())
            .filter_map(|(property, declaration)| {
                Some(MemberDelegateCandidate {
                    declaration: MemberDelegateDeclaration::Property(property as u32),
                    field: declaration.delegate_field?,
                    getter: declaration.getter?,
                    last_accessor: declaration.setter.unwrap_or(declaration.getter?),
                    receivers: vec![owner_receiver],
                })
            })
            .collect::<Vec<_>>();
        candidates.extend(
            ir.member_ext_props
                .get(&owner)
                .into_iter()
                .flatten()
                .enumerate()
                .filter_map(|(property, declaration)| {
                    Some(MemberDelegateCandidate {
                        declaration: MemberDelegateDeclaration::Extension(property as u32),
                        field: declaration.delegate_field?,
                        getter: declaration.getter,
                        last_accessor: declaration.setter.unwrap_or(declaration.getter),
                        receivers: vec![
                            owner_receiver,
                            approximate_delegate_receiver(
                                declaration.receiver,
                                declaration
                                    .type_params
                                    .iter()
                                    .map(|parameter| parameter.semantic_name.as_str()),
                                owner_type_parameters.iter().map(String::as_str),
                            ),
                        ],
                    })
                }),
        );
        for candidate in candidates {
            let MemberDelegateCandidate {
                declaration,
                field,
                getter,
                last_accessor,
                receivers,
            } = candidate;
            let Some((store, delegate)) = delegate_store(ir, class, field) else {
                continue;
            };
            let Some(kind) = in_place_delegate(ir, delegate) else {
                continue;
            };
            let stored = &ir.classes[class as usize].fields[field as usize];
            let (storage_ty, line) = (stored.ty, stored.constructor_store_line);
            replace_operations(
                ir,
                |expression| {
                    matches!(expression, IrExpr::GetField { class: owner_class, index, .. }
                    if *owner_class == class && *index == field)
                },
                delegate,
            );
            retire_store(ir, store, delegate, kind, line);
            match declaration {
                MemberDelegateDeclaration::Property(property) => {
                    ir.classes[class as usize].properties[property as usize].delegate_field = None;
                }
                MemberDelegateDeclaration::Extension(property) => {
                    ir.member_ext_props
                        .get_mut(&owner)
                        .expect("a collected member extension keeps its owner")
                        [property as usize]
                        .delegate_field = None;
                }
            }
            removed.entry(class).or_default().insert(field);
            let method = add_delegate_method(
                ir,
                DelegateMethod {
                    name: format!("{}$delegate", ir.functions[getter as usize].name),
                    receivers,
                    delegate,
                    storage_ty,
                    line,
                    source_order_of: getter,
                },
            );
            let methods = &mut ir.classes[class as usize].methods;
            let position = methods
                .iter()
                .position(|&function| function == last_accessor)
                .map_or(methods.len(), |index| index + 1);
            let position = if last_accessor == getter
                && methods
                    .get(position)
                    .is_some_and(|method| annotation_markers.contains(method))
            {
                position + 1
            } else {
                position
            };
            methods.insert(position, method);
            match declaration {
                MemberDelegateDeclaration::Property(property) => {
                    ir.record_jvm_member_delegate_method(class, property, method, getter);
                }
                MemberDelegateDeclaration::Extension(property) => {
                    ir.record_jvm_member_extension_delegate_method(class, property, method, getter);
                }
            }
        }
    }
    crate::jvm::field_compaction::remove_fields(ir, &removed);
}

/// A generated delegate method is not itself generic. Replace only the property's own type
/// parameters by their bounds: the surrounding receiver remains available to generic-signature
/// emission (`List<T>` becomes `List<Any>`, not raw `List`) and a concrete nullable value class
/// remains boxed when value-class lowering selects its physical slot.
fn approximate_delegate_receiver<'a>(
    receiver: Ty,
    own_type_parameters: impl Iterator<Item = &'a str>,
    enclosing_type_parameters: impl Iterator<Item = &'a str>,
) -> Ty {
    let parameters = own_type_parameters
        .chain(enclosing_type_parameters)
        .map(str::to_owned)
        .collect::<HashSet<_>>();
    approximate_type_parameters(receiver, &parameters, &mut HashSet::default())
}

/// The dispatch receiver of a static delegate method cannot refer to its owner's class type
/// parameters. Keep the applied owner shape, substituting each argument with its declared primary
/// bound, so `Host<A>` becomes `Host<Any>` rather than raw `Host` in the method Signature.
fn approximate_owner_receiver(ir: &IrFile, owner: crate::types::TypeName) -> Ty {
    let Some(signature) = ir.class_signature_name(owner) else {
        return Ty::Obj(owner, &[]);
    };
    if signature.type_params.is_empty() {
        return Ty::Obj(owner, &[]);
    }
    let parameters = signature
        .type_params
        .iter()
        .map(|parameter| parameter.semantic_name.clone())
        .collect::<HashSet<_>>();
    let arguments = signature
        .type_params
        .iter()
        .map(|parameter| {
            let bound = crate::jvm::generic_erasure::declared_primary_bound(parameter)
                .unwrap_or_else(|| Ty::obj_name(crate::types::wk::any()));
            approximate_type_parameters(bound, &parameters, &mut HashSet::default()).non_null()
        })
        .collect::<Vec<_>>();
    Ty::obj_args_name(owner, &arguments)
}

fn approximate_type_parameters(
    ty: Ty,
    parameters: &HashSet<String>,
    visiting: &mut HashSet<String>,
) -> Ty {
    match ty {
        Ty::TyParam(name, bound) if parameters.contains(name) => {
            if !visiting.insert(name.to_owned()) {
                return Ty::obj_name(crate::types::wk::any());
            }
            let approximated = approximate_type_parameters(*bound, parameters, visiting).non_null();
            visiting.remove(name);
            approximated
        }
        Ty::Fun(signature) => Ty::fun_with_shape(
            signature
                .params
                .iter()
                .map(|parameter| approximate_type_parameters(*parameter, parameters, visiting))
                .collect(),
            approximate_type_parameters(signature.ret, parameters, visiting),
            signature.context_count,
            signature.has_receiver,
            signature.suspend,
        ),
        Ty::Nullable(inner) => {
            Ty::nullable(approximate_type_parameters(*inner, parameters, visiting))
        }
        Ty::PlatformNullable(inner) => {
            Ty::platform_nullable(approximate_type_parameters(*inner, parameters, visiting))
        }
        Ty::InProjection(inner) => {
            Ty::in_projection(approximate_type_parameters(*inner, parameters, visiting))
        }
        Ty::OutProjection(inner) => {
            Ty::out_projection(approximate_type_parameters(*inner, parameters, visiting))
        }
        Ty::StarProjection(inner) => {
            Ty::star_projection(approximate_type_parameters(*inner, parameters, visiting))
        }
        Ty::DefinitelyNotNull(inner) => {
            approximate_type_parameters(*inner, parameters, visiting).non_null()
        }
        Ty::Obj(classifier, arguments) if !arguments.is_empty() => Ty::obj_args_name(
            classifier,
            &arguments
                .iter()
                .map(|argument| approximate_type_parameters(*argument, parameters, visiting))
                .collect::<Vec<_>>(),
        ),
        Ty::Intersection(parts) => Ty::intersection(
            &parts
                .iter()
                .map(|part| approximate_type_parameters(*part, parameters, visiting))
                .collect::<Vec<_>>(),
        ),
        _ => ty,
    }
}

#[derive(Clone, Copy)]
enum MemberDelegateDeclaration {
    Property(u32),
    Extension(u32),
}

struct MemberDelegateCandidate {
    declaration: MemberDelegateDeclaration,
    field: u32,
    getter: FunId,
    last_accessor: FunId,
    /// Dispatch receiver followed by the extension receiver, when present.
    receivers: Vec<Ty>,
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

/// Rewrite every expression `retired` matches into the delegate itself. The delegate is a leaf, so
/// each read becomes an independent copy that keeps the read's own identity and debug facts.
fn replace_operations(ir: &mut IrFile, retired: impl Fn(&IrExpr) -> bool, delegate: ExprId) {
    let value = ir.expr(delegate).clone();
    for expression in &mut ir.exprs {
        if retired(expression) {
            *expression = value.clone();
        }
    }
}

/// The delegate field's declaration store becomes the singleton's own evaluation at the same line,
/// or disappears for a constant.
fn retire_store(
    ir: &mut IrFile,
    store: ExprId,
    delegate: ExprId,
    kind: InPlaceDelegate,
    line: u32,
) {
    ir.property_initializer_stores.remove(&store);
    // The store node no longer writes the field, whether or not a statement still reaches it.
    ir.exprs[store as usize] = ir.expr(delegate).clone();
    match kind {
        InPlaceDelegate::Singleton => {
            ir.record_jvm_delegate_evaluation(store);
            if line != 0 {
                ir.expr_source_lines.insert(store, line);
            }
        }
        InPlaceDelegate::Constant => {
            for expression in &mut ir.exprs {
                if let IrExpr::Block { stmts, .. } = expression {
                    stmts.retain(|&statement| statement != store);
                }
            }
        }
    }
}

/// The source line a generated getter's value is stated on: its property's declaration line.
fn accessor_line(ir: &IrFile, getter: u32) -> u32 {
    let Some(body) = ir.functions[getter as usize].body else {
        return 0;
    };
    match ir.expr(body) {
        IrExpr::Block { stmts, .. } => stmts
            .iter()
            .find_map(|statement| ir.expr_source_lines.get(statement).copied())
            .unwrap_or(0),
        _ => 0,
    }
}

struct DelegateMethod {
    name: String,
    /// The owner (a member property) or extension receiver the method takes as `<this>`.
    receivers: Vec<Ty>,
    delegate: ExprId,
    storage_ty: Ty,
    line: u32,
    /// The accessor whose source position the method shares; it follows that property's accessors.
    source_order_of: u32,
}

/// `private static Object get<X>$delegate([<this>])`, returning the delegate.
fn add_delegate_method(ir: &mut IrFile, method: DelegateMethod) -> u32 {
    let result = Ty::nullable(Ty::obj("kotlin/Any"));
    let value = ir.add_expr(ir.expr(method.delegate).clone());
    ir.logical_types.insert(value, method.storage_ty);
    let value = if method.storage_ty.is_jvm_scalar() {
        ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: value,
            type_operand: result,
        })
    } else {
        value
    };
    let returned = ir.add_expr(IrExpr::Return(Some(value)));
    if method.line != 0 {
        // The line opens the method: it is stated on the delegate, not on the `areturn`.
        ir.expr_source_lines.insert(value, method.line);
        ir.expr_source_lines.insert(returned, method.line);
    }
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![returned],
        value: None,
    });
    let params = method.receivers;
    let parameter_count = params.len();
    let function = ir.add_fun(IrFunction {
        name: method.name,
        params,
        ret: result,
        body: Some(body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    ir.fn_params.insert(
        function,
        FnParamInfo::identities(
            (0..parameter_count)
                .map(|_| {
                    IrParameterIdentity::generated(
                        IrGeneratedParameterRole::DelegateMethodReceiver,
                        None,
                    )
                })
                .collect(),
        ),
    );
    // Its `<this>` parameter is a debug local, as in any generated accessor.
    ir.fn_debug_locals.insert(function);
    ir.method_visibilities.insert(function, Visibility::Private);
    // kotlinc writes it `private static`, without `final`.
    ir.open_methods.insert(function);
    if let Some(order) = ir.fn_source_order.get(&method.source_order_of).copied() {
        ir.fn_source_order.insert(function, order);
    }
    function
}
