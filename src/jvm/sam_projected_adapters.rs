//! JVM parameter storage for adapters over contravariantly projected SAM methods.
//!
//! Checked FIR and common IR retain the function's logical input type (`String` for
//! `Sink<in String>`). The JVM implementation method selected by `LambdaMetafactory` receives the
//! abstract declaration's erased slot instead (`Object` for `Sink<T>.accept(T)`). This pass commits
//! only those recorded contravariant parameters to that physical representation.

use crate::ir::{IrExpr, IrFile};

pub(super) fn realize(ir: &mut IrFile) {
    let adapters = ir
        .exprs
        .iter()
        .filter_map(|expression| {
            let IrExpr::Lambda {
                impl_fn,
                captures,
                sam: Some(target),
                ..
            } = expression
            else {
                return None;
            };
            target
                .contravariant_parameters
                .iter()
                .any(|parameter| *parameter)
                .then(|| {
                    (
                        *impl_fn,
                        captures.len(),
                        target.contravariant_parameters.clone(),
                        target.declared_parameters.clone(),
                    )
                })
        })
        .collect::<Vec<_>>();

    for (implementation, captures, contravariant, declared) in adapters {
        let function = ir
            .functions
            .get_mut(implementation as usize)
            .expect("a SAM adapter names its implementation function");
        assert_eq!(
            contravariant.len(),
            declared.len(),
            "a SAM capture flag is recorded for every declared parameter"
        );
        assert_eq!(
            function.params.len(),
            captures + declared.len(),
            "a SAM adapter implementation contains its captures and method parameters"
        );
        for (ordinal, projected) in contravariant.into_iter().enumerate() {
            if projected {
                function.params[captures + ordinal] =
                    super::bridges::bridge_erasure(declared[ordinal]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IrFunction, IrSamMethod, IrSamTarget};
    use crate::types::Ty;

    #[test]
    fn only_a_contravariant_parameter_takes_the_declarations_erased_slot() {
        let mut ir = IrFile::default();
        let implementation = ir.add_fun(IrFunction {
            name: "adapter".to_string(),
            params: vec![Ty::fun(vec![Ty::String], Ty::Unit), Ty::String, Ty::Int],
            ret: Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        ir.add_expr(IrExpr::Lambda {
            impl_fn: implementation,
            arity: 2,
            captures: vec![0],
            sam: Some(IrSamTarget {
                classifier: crate::types::type_name("neutral/Sink"),
                method: "accept".to_string(),
                method_target: IrSamMethod::FunctionTypeInvoke,
                parameters: vec![Ty::String, Ty::Int],
                contravariant_parameters: vec![true, false],
                result: Ty::Unit,
                declared_parameters: vec![Ty::ty_param("T", Ty::obj("kotlin/Any")), Ty::Int],
                declared_result: Ty::Unit,
                context_count: 0,
                has_receiver: false,
                suspend: false,
                source_suspend: false,
                overridden_results: Vec::new(),
                function_adapter: false,
                wraps_function_value: true,
                nullable: false,
                kotlin_interface: false,
                parameter_identities: Vec::new(),
            }),
            inline_body: None,
        });

        realize(&mut ir);

        assert_eq!(
            ir.functions[implementation as usize].params,
            vec![
                Ty::fun(vec![Ty::String], Ty::Unit),
                Ty::obj("kotlin/Any"),
                Ty::Int,
            ]
        );
    }
}
