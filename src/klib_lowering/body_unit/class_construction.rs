//! Materialized dependency storage and its private initialization frame in this closed unit.

use super::*;
use crate::ir::{IrClass, IrCtorArg, IrCtorParameterProvenance, IrExpr, IrField};

impl<'u, 's> Linker<'u, 's> {
    pub(super) fn materialize_class(
        &mut self,
        ir: &mut IrFile,
        identity: &KlibDependencyCallable,
        selected: KlibBodyCallable<'s>,
    ) -> Result<(), KlibBodyDeclineReason> {
        if self.classes.contains_key(identity) {
            return Ok(());
        }
        let Some(plan) = super::super::class_initialization::plan(selected, self.sources.bodies)?
        else {
            return Ok(());
        };
        let classifier = plan.header.contract.classifier;
        if ir.class_id_by_name(classifier).is_some() {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "different dependency identities claim the same materialized classifier".to_owned(),
            ));
        }
        let header = super::super::function_lowering::initializer_header(&plan)?;
        let mut class = IrClass::synthetic(classifier);
        class.supertypes.push(Ty::obj_name(crate::types::wk::any()));
        class.fields = plan
            .fields
            .iter()
            .map(|field| {
                IrField::new(field.declaration.name.clone(), field.ty)
                    .with_is_final(field.final_)
                    .with_is_private(field.private)
            })
            .collect();
        class.ctor_args = plan
            .header
            .function
            .regular_parameters
            .iter()
            .zip(header.params())
            .map(|(parameter, ty)| IrCtorArg {
                name: Some(parameter.name.clone()),
                context_kind: crate::types::ContextParameterKind::None,
                ty: *ty,
                declared_ty: Some(*ty),
                is_field: false,
                field_index: None,
                has_default: false,
                is_vararg: false,
                type_param: None,
                check: None,
                anonymous_super_forward: None,
                capture: None,
                provenance: IrCtorParameterProvenance::Value,
                capture_identity: None,
            })
            .collect();
        // The initialization frame stores actual fields explicitly. Its plain value parameters
        // describe constructor arity, not leading constructor-backed storage.
        class.ctor_param_count = 0;
        class.explicit_param_stores = true;
        let class = ir.add_class(class);
        let function = declare_function(ir, &header);
        ir.functions[function as usize].is_static = false;
        ir.functions[function as usize].dispatch_receiver = Some(classifier);
        ir.set_method_visibility(function, crate::types::Visibility::Private);
        ir.classes[class as usize].methods.push(function);
        ir.note_class_method(class, function);
        let receiver = ir.add_expr(IrExpr::GetValue(0));
        ir.set_logical_type(receiver, Ty::obj_name(classifier));
        let args = header
            .params()
            .iter()
            .enumerate()
            .map(|(index, ty)| {
                let value = ir.add_expr(IrExpr::GetValue(
                    u32::try_from(index + 1).expect("a value index fits u32"),
                ));
                ir.set_logical_type(value, *ty);
                Some(value)
            })
            .collect();
        let initialize = ir.add_expr(IrExpr::MethodCall {
            class,
            index: 0,
            receiver,
            args,
        });
        ir.set_logical_type(initialize, Ty::Unit);
        ir.classes[class as usize].init_body = Some(initialize);
        self.classes.insert(identity.clone(), class);
        self.pending.push_back(PendingBody {
            identity: identity.clone(),
            arena: plan.header.arena,
            symbol: &plan.header.function.base.symbol,
            statements: &[],
            header,
            function,
            initialization: Some((class, plan)),
        });
        Ok(())
    }
}
