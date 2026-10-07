//! Typed static-function calls through their declaration's private-access boundary.

use super::*;

impl Emitter<'_> {
    pub(super) fn emit_static_function_call(
        &mut self,
        expression: ExprId,
        function: u32,
        class: Option<TypeName>,
        args: &[ExprId],
        code: &mut CodeBuilder,
    ) {
        let declaration = &self.ir.functions[function as usize];
        let parameters = jvm_function_params(self.ir, function);
        let result = jvm_declared_ty(&declaration.ret);
        if let Err(mismatch) =
            self.emit_source_call_operands(expression, 0, args, &parameters, code)
        {
            self.bail_descriptor_arity(&mismatch, result, code);
            return;
        }
        // Lambda placement can move a receiverless implementation from the facade onto the class
        // whose bytecode retains its method handle. Calls built before that JVM-only placement —
        // notably a suspend helper's generated continuation — still carry `Callee::Local`. The
        // explicit physical-owner table is authoritative; never recover the owner from a generated
        // method or continuation name.
        let class = class.or_else(|| self.ir.class_static_local_functions.get(&function).copied());
        let (owner, is_interface, emitted_by_owner) =
            match self.local_delegate_access.foreign_owner(function) {
                Some(owner) => (owner.classifier.render(), owner.is_interface, false),
                None => match class {
                    Some(class) => {
                        let owner = StaticOwner::Class(class);
                        (
                            class.render(),
                            owner.is_interface(self.ir)
                                || self.bodies.owner_is_interface_name(class),
                            self.static_owner == Some(owner),
                        )
                    }
                    None => (
                        self.facade.clone(),
                        false,
                        self.static_owner == Some(StaticOwner::Facade),
                    ),
                },
            };
        let name = if static_accessors::routes_through_accessor(
            self.ir,
            self.local_delegate_access,
            emitted_by_owner,
            function,
        ) || static_accessors::inline_exports_private_call(
            self.ir,
            self.export_private_calls,
            function,
        ) {
            format!("access${}", declaration.name)
        } else {
            declaration.name.clone()
        };
        let descriptor = method_descriptor(&parameters, result);
        let method = if is_interface {
            self.cw.interface_methodref(&owner, &name, &descriptor)
        } else {
            self.cw.methodref(&owner, &name, &descriptor)
        };
        let words = parameters.iter().map(|ty| slot_words(*ty) as i32).sum();
        self.mark_call_start(expression, code);
        code.invokestatic(method, words, physical_call_result_words(result));
    }
}
