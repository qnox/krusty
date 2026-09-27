//! kotlinc's synthetic accessors for PROTECTED members declared in another package.
//!
//! Kotlin lets code written inside a subclass call its supertypes' protected members on a receiver
//! of that subclass, lambdas and anonymous objects there included. The JVM grants a protected
//! member only to its own package and to the code of a subclass whose receiver is that subclass.
//! kotlinc's `SyntheticAccessorLowering` therefore reaches an inaccessible protected member through
//! a `public static final synthetic access$<name>` declared by the class of the call's receiver:
//! it takes that receiver as `$this`, the member's parameters as the receiver sees them, and calls
//! the member on it.

use super::*;
use crate::ir::{IrModuleProperty, IrProtectedMemberCall};
use crate::jvm::private_static_access::StaticOwner;

/// One protected-member accessor: `access$<target>` of class `owner`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(in crate::jvm) struct ProtectedAccessor {
    /// The class of the call's receiver, which declares the accessor.
    pub(in crate::jvm) owner: TypeName,
    target: String,
    /// The accessor's parameters after `$this` and its result, as the receiver sees the member.
    pub(in crate::jvm) parameters: Vec<Ty>,
    pub(in crate::jvm) result: Ty,
    /// The member's own descriptor, which the accessor's call names.
    target_parameters: Vec<Ty>,
    target_result: Ty,
    /// The local-variable names of the accessor's parameters after `$this`.
    parameter_names: Vec<Option<String>>,
}

impl ProtectedAccessor {
    pub(in crate::jvm) fn name(&self) -> String {
        format!("access${}", self.target)
    }

    pub(in crate::jvm) fn descriptor(&self) -> String {
        let mut parameters = Vec::with_capacity(self.parameters.len() + 1);
        parameters.push(Ty::obj_name(self.owner));
        parameters.extend(self.parameters.iter().copied());
        method_descriptor(&parameters, self.result)
    }

    /// The accessor of member `target` of declared `parameters` and `result` that `protected` names.
    fn of(
        ir: &IrFile,
        protected: &IrProtectedMemberCall,
        target: &str,
        parameters: &[Ty],
        result: Ty,
    ) -> Self {
        let seen = parameters
            .iter()
            .map(|parameter| jvm_declared_ty(&protected.substitute(ir, *parameter)))
            .collect::<Vec<_>>();
        let semantic = parameters
            .iter()
            .map(|parameter| protected.substitute(ir, *parameter))
            .collect::<Vec<_>>();
        let parameter_names = crate::jvm::parameter_names::resolved_local_variables(
            &protected.parameter_identities,
            &semantic,
            target,
        );
        Self {
            owner: protected.receiver,
            target: target.to_owned(),
            parameters: seen,
            result: jvm_declared_ty(&protected.substitute(ir, result)),
            target_parameters: jvm_tys(parameters),
            target_result: jvm_declared_ty(&result),
            parameter_names,
        }
    }
}

/// Whether `class` is `ancestor` or inherits from it.
fn is_subclass(ir: &IrFile, class: TypeName, ancestor: TypeName) -> bool {
    class == ancestor
        || ir
            .classifier_hierarchies
            .get(&class)
            .is_some_and(|hierarchy| {
                hierarchy
                    .iter()
                    .any(|applied| applied.classifier == ancestor)
            })
}

/// Whether code of `context` may call the member of `call` itself, by the JVM's protected access
/// rule as kotlinc's `SyntheticAccessorLowering.isAccessible` states it: from the member's own
/// package, or from a subclass of its class on a receiver of that subclass. The receiver's class
/// must be one this file declares, since only the file declaring a class adds members to it.
fn needs_accessor(
    ir: &IrFile,
    context: StaticOwner,
    facade: &str,
    call: &IrProtectedMemberCall,
) -> bool {
    let accessible = match context {
        StaticOwner::Class(class) => {
            class.parent() == call.declaring.parent()
                || (is_subclass(ir, class, call.declaring) && is_subclass(ir, call.receiver, class))
        }
        StaticOwner::Facade => call
            .declaring
            .package_matches(facade.rsplit_once('/').map_or("", |(package, _)| package)),
    };
    !accessible
        && ir
            .classes
            .iter()
            .any(|class| class.fq_name == call.receiver)
}

/// The accessor through which code of `context` makes the protected member call `call`, or `None`
/// when that code may call the member itself.
pub(super) fn protected_accessor(
    ir: &IrFile,
    context: StaticOwner,
    facade: &str,
    call: crate::ir::ExprId,
) -> Option<ProtectedAccessor> {
    let protected = ir.protected_member_calls.get(&call)?;
    let IrExpr::Call {
        callee:
            Callee::Virtual {
                name,
                params: Some((parameters, result)),
                ..
            },
        ..
    } = ir.expr(call)
    else {
        return None;
    };
    needs_accessor(ir, context, facade, protected)
        .then(|| ProtectedAccessor::of(ir, protected, name, parameters, *result))
}

/// The getter and setter accessors a property-reference carrier of file `facade` calls for the
/// protected accessors of `property` that `reference` reads and writes. A carrier is a class of its
/// own, never a subclass of the property's class.
pub(in crate::jvm) fn property_reference_accessors(
    ir: &IrFile,
    facade: &str,
    reference: crate::ir::ExprId,
    property: &IrModuleProperty,
    mutable: bool,
) -> (Option<ProtectedAccessor>, Option<ProtectedAccessor>) {
    let Some(protected) = ir
        .protected_member_calls
        .get(&reference)
        .filter(|protected| needs_accessor(ir, StaticOwner::Facade, facade, protected))
    else {
        return (None, None);
    };
    let getter = (property.visibility == crate::types::Visibility::Protected).then(|| {
        let reader = IrProtectedMemberCall {
            parameter_identities: Vec::new(),
            ..protected.clone()
        };
        let name = crate::jvm::module_calls::property_getter_name(property);
        ProtectedAccessor::of(ir, &reader, &name, &[], property.ty)
    });
    let setter = (mutable && property.setter_visibility == crate::types::Visibility::Protected)
        .then(|| {
            let name = crate::names::property_setter_name(&property.name);
            ProtectedAccessor::of(ir, protected, &name, &[property.ty], Ty::Unit)
        });
    (getter, setter)
}

/// Convert the value on the stack from `from` to `to`: box or unbox a scalar, narrow a reference.
fn adapt(cw: &mut ClassWriter, code: &mut CodeBuilder, from: Ty, to: Ty) {
    if type_descriptor(from) == type_descriptor(to) {
        return;
    }
    if from.is_jvm_scalar() && to.is_reference() {
        box_prim_free(cw, code, from);
    } else if from.is_reference() && to.is_jvm_scalar() {
        unbox_prim_from(cw, code, from, to);
    } else if from.is_reference() && to.is_reference() {
        let internal = crate::jvm::names::instanceof_internal_name(to);
        if internal != "java/lang/Object" {
            let class = cw.class_ref(&internal);
            code.checkcast(class);
        }
    }
}

/// Write `accessor` into its owner's class `cw`: load `$this` and each parameter, adapt each to the
/// member's own descriptor, call the member on `$this`, and adapt its result back. The body maps
/// to the owner's `declaration_line` from the call on, as kotlinc's does.
pub(super) fn emit(
    accessor: &ProtectedAccessor,
    declaration_line: u32,
    flags: u16,
    cw: &mut ClassWriter,
) {
    let name = accessor.name();
    let descriptor = accessor.descriptor();
    let words = 1 + accessor
        .parameters
        .iter()
        .map(|ty| slot_words(*ty))
        .sum::<u16>();
    let mut code = CodeBuilder::new(words);
    let owner = accessor.owner.render();
    let mut locals = vec![("$this".to_string(), format!("L{owner};"), 0)];
    code.aload(0);
    let mut slot = 1u16;
    for ((&parameter, &target), local) in accessor
        .parameters
        .iter()
        .zip(&accessor.target_parameters)
        .zip(&accessor.parameter_names)
    {
        load(parameter, slot, &mut code);
        adapt(cw, &mut code, parameter, target);
        if let Some(local) = local {
            locals.push((local.clone(), type_descriptor(parameter), slot));
        }
        slot += slot_words(parameter);
    }
    code.mark_line(declaration_line);
    let target_descriptor = method_descriptor(&accessor.target_parameters, accessor.target_result);
    let method = cw.methodref(&owner, &accessor.target, &target_descriptor);
    let argument_words = accessor
        .target_parameters
        .iter()
        .map(|ty| slot_words(*ty) as i32)
        .sum::<i32>();
    code.invokevirtual(
        method,
        argument_words,
        slot_words(accessor.target_result) as i32,
    );
    adapt(cw, &mut code, accessor.target_result, accessor.result);
    emit_return(accessor.result, &mut code);
    code.ensure_locals(words);
    code.link();
    cw.add_method(flags, &name, &descriptor, &code);
    cw.set_method_debug(&name, &descriptor, None, &locals);
}

impl Emitter<'_> {
    /// The accessor through which the class being emitted makes protected member call `call`.
    pub(super) fn protected_accessor(&self, call: crate::ir::ExprId) -> Option<ProtectedAccessor> {
        if !self.ir.protected_member_calls.contains_key(&call) {
            return None;
        }
        let context = self
            .ir
            .classes
            .iter()
            .find(|class| class.fq_name.matches(&self.owner))
            .map_or(StaticOwner::Facade, |class| {
                StaticOwner::Class(class.fq_name)
            });
        protected_accessor(self.ir, context, &self.facade, call)
    }

    /// The result of member call `call` of declared result `result` as the stack holds it: an
    /// accessor returns the member's result as the receiver sees it.
    pub(super) fn member_call_result(&self, call: crate::ir::ExprId, result: &Ty) -> Ty {
        match self.protected_accessor(call) {
            Some(_) => self.ir.protected_member_calls[&call].substitute(self.ir, *result),
            None => *result,
        }
    }

    /// Call `accessor` in place of member call `call` on `receiver` with `arguments`.
    pub(super) fn emit_protected_accessor_call(
        &mut self,
        call: crate::ir::ExprId,
        receiver: crate::ir::ExprId,
        arguments: &[crate::ir::ExprId],
        accessor: &ProtectedAccessor,
        code: &mut CodeBuilder,
    ) {
        // Common lowering materialized each argument at the member's declared parameter type; an
        // accessor parameter the receiver specializes takes the value before that coercion.
        let mut operands = Vec::with_capacity(arguments.len() + 1);
        operands.push(receiver);
        operands.extend(
            arguments
                .iter()
                .zip(&accessor.parameters)
                .zip(&accessor.target_parameters)
                .map(
                    |((&argument, &parameter), &target)| match self.ir.expr(argument) {
                        IrExpr::TypeOp {
                            op: crate::ir::IrTypeOp::ImplicitCoercion,
                            arg,
                            ..
                        } if type_descriptor(parameter) != type_descriptor(target) => *arg,
                        _ => argument,
                    },
                ),
        );
        let mut physical = Vec::with_capacity(operands.len());
        physical.push(Ty::obj_name(accessor.owner));
        physical.extend(accessor.parameters.iter().copied());
        if let Err(mismatch) = self.emit_source_call_operands(call, 1, &operands, &physical, code) {
            self.bail_descriptor_arity(&mismatch, accessor.result, code);
            return;
        }
        let words = physical
            .iter()
            .map(|ty| slot_words(*ty) as i32)
            .sum::<i32>();
        let method = self.cw.methodref(
            &accessor.owner.render(),
            &accessor.name(),
            &accessor.descriptor(),
        );
        self.mark_call_start(call, code);
        code.invokestatic(method, words, physical_call_result_words(accessor.result));
    }
}
