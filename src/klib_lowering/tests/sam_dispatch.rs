//! Full producer declarations become SAM slot headers without rooting abstract/default bodies.

use super::*;
use crate::backend::CheckedBackendCallables;
use crate::ir::{IrSamMethod, IrSamTarget};

const LIBRARY: &str = include_str!("../../../tests/fixtures/klib_sam_dispatch/Library.kt");

fn method(libraries: &KlibLibraries, owner: &str, name: &str) -> crate::fir::ExternalCallableId {
    let owner = libraries.classifier(type_name(owner)).unwrap();
    owner.declared_callables[name].functions()[0]
        .callable
        .external_identity
        .unwrap()
}

fn sam(ir: &IrFile) -> (crate::ir::ExprId, IrSamTarget) {
    let targets = ir
        .exprs
        .iter()
        .enumerate()
        .filter_map(|(id, node)| match node {
            IrExpr::Lambda {
                sam: Some(target), ..
            } => Some((id as u32, target.as_ref().clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    let [target] = &targets[..] else {
        panic!("one real producer SAM: {targets:?}")
    };
    target.clone()
}

#[test]
fn official_sam_dispatch_families_preserve_headers_frames_and_copy_without_body_obligations() {
    let Some((work, libraries)) = super::sam_conversions::wasm_library(LIBRARY, "sam_dispatch")
    else {
        return;
    };
    let left = method(&libraries, "fixture/Left", "apply");
    let right = method(&libraries, "fixture/Right", "apply");
    let specialized = method(&libraries, "fixture/Specialized", "apply");
    let defaulted = method(&libraries, "fixture/Defaulted", "apply");
    let note = method(&libraries, "fixture/Defaulted", "note");
    for (name, declarations, defaults, inline) in [
        ("diamond", vec![left, right], vec![], false),
        ("specialized", vec![specialized, left], vec![], false),
        ("defaulted", vec![defaulted], vec![note], false),
        ("inlineSpecialized", vec![specialized, left], vec![], true),
    ] {
        let symbols = libraries.symbols(SymbolNamespace::Package(type_name("fixture")), name);
        let [function] = symbols.callables.functions() else {
            panic!("one producer factory")
        };
        let selected = libraries
            .external_callable(function.callable.external_identity.unwrap())
            .unwrap();
        let callable = selected.klib_body_callable().unwrap();
        let checked = crate::resolve::for_checked_klib_bodies(
            &libraries,
            libraries.declaration_bodies(),
            [callable],
            &[],
        )
        .unwrap();
        let callees = KlibCalleeFacts::new([callable]).with_checked_dependency_facts(&checked);
        let externals = super::sam_conversions::checked_member_externals(&libraries, &checked);
        let mut unit = DependencyBodyUnit::default();
        unit.lower_function_with_intrinsics_and_externals(
            callable,
            libraries.declaration_bodies(),
            &callees,
            libraries.intrinsic_facts(),
            &externals,
        )
        .unwrap();
        let mut ir = unit.into_ir();
        let (expression, target) = sam(&ir);
        assert_eq!(
            target
                .dispatch_methods
                .iter()
                .map(|frame| frame.method)
                .collect::<Vec<_>>(),
            declarations
                .iter()
                .map(|&id| Some(IrSamMethod::External(id)))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            target
                .default_methods
                .iter()
                .map(|frame| frame.method)
                .collect::<Vec<_>>(),
            defaults
                .iter()
                .map(|&id| Some(IrSamMethod::External(id)))
                .collect::<Vec<_>>()
        );
        if name == "specialized" || inline {
            assert_eq!(target.dispatch_methods[0].parameters, vec![Ty::Int]);
            assert!(
                matches!(target.dispatch_methods[1].parameters.as_slice(), [parameter] if matches!(parameter.kind(), TyKind::TyParam(..)))
            );
        }
        let facts = CheckedBackendCallables::freeze(&ir, &libraries).unwrap();
        for &declaration in declarations.iter().chain(&defaults) {
            assert!(
                facts.callable(declaration).is_none(),
                "implemented/inherited headers are not calls"
            );
            assert!(facts.sam_declaration(declaration).is_some());
            assert!(facts.declaration_identity(declaration).is_some());
        }
        // The SAM headers add no body obligation. The only dependency body is the target member
        // the specialized lambdas call (`Int.plus`), one of the member edges the checker froze.
        let bodies = facts
            .klib_body_callables()
            .into_iter()
            .map(|callable| callable.identity().clone())
            .collect::<Vec<_>>();
        assert_eq!(
            bodies.len(),
            usize::from(name == "specialized" || inline),
            "{name}: {bodies:?}"
        );
        for body in &bodies {
            assert!(
                checked.member_edges().contains_key(body),
                "{name}: {body:?}"
            );
            for &declaration in declarations.iter().chain(&defaults) {
                assert_ne!(
                    libraries
                        .external_callable(declaration)
                        .unwrap()
                        .dependency_callable
                        .as_ref(),
                    Some(body),
                    "{name}: a SAM header became a body obligation"
                );
            }
        }
        let copied = crate::ir::clone_expression_with_own_lambdas(&mut ir, expression);
        let IrExpr::Lambda {
            sam: Some(copied_target),
            ..
        } = ir.expr(copied)
        else {
            panic!("copied SAM")
        };
        assert_eq!(copied_target.as_ref(), &target);
        validated(&ir);
        if inline {
            let mut prepared = IrFile::default();
            lower_inline_template(
                &mut prepared,
                &selected,
                libraries.inline_declaration_bodies(),
                TemplateCallees {
                    declarations: libraries.declaration_bodies(),
                    references: libraries.reference_bindings(),
                    source: &libraries,
                    dependency_facts: &checked,
                },
            )
            .unwrap()
            .unwrap();
            assert_eq!(sam(&prepared).1.dispatch_methods, target.dispatch_methods);
        }
        if let Some(&default) = defaults.first() {
            // Only an actual call introduces this default body's ordinary dependency obligation.
            let call = ir.add_expr(IrExpr::Call {
                callee: Box::new(crate::ir::Callee::External {
                    target: default,
                    default_provider: None,
                    params: vec![],
                    ret: Ty::String,
                    substitutions: vec![],
                    defaults: vec![],
                    extension_receiver_parameter: None,
                }),
                dispatch_receiver: Some(expression),
                args: vec![],
            });
            ir.add_fun(crate::ir::IrFunction {
                name: "callDefault".into(),
                params: vec![],
                ret: Ty::String,
                body: Some(call),
                is_static: true,
                dispatch_receiver: None,
                param_checks: vec![],
            });
            let actual = CheckedBackendCallables::freeze(&ir, &libraries).unwrap();
            assert!(actual.callable(default).is_some());
            let identity = actual.declaration_identity(default).unwrap();
            assert!(actual
                .klib_body_callables()
                .iter()
                .any(|body| body.identity() == identity));
        }
    }
    std::fs::remove_dir_all(work).unwrap();
}
