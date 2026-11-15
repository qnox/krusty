use super::*;
use krusty::backend::BackendClassifierFact;
use krusty::fir::CallableId;
use krusty::ir::IrFunction;
use krusty::types::{type_name, ClassifierDeclarationName};
use std::sync::Arc;

struct Names;

impl BackendClassifierSource for Names {
    fn classifier(&self, _: TypeName) -> Option<Arc<BackendClassifierFact>> {
        None
    }

    fn classifier_declaration_name(
        &self,
        declaration: krusty::libraries::ClassifierDeclarationKey,
    ) -> Option<Option<ClassifierDeclarationName>> {
        (declaration
            == krusty::libraries::ClassifierDeclarationKey::Ordinary(type_name("pkg/Scope$Base")))
        .then(|| {
            Some(ClassifierDeclarationName::Nested {
                owner: type_name("pkg/Scope"),
                name: "Base$Part".into(),
            })
        })
    }
}

fn request() -> AccessorRequest<'static> {
    AccessorRequest {
        parent: type_name("pkg/Child"),
        qualifier: type_name("pkg/Scope$Base"),
        invocation_owner: type_name("java/lang/Object"),
        kind: IrSuperCallKind::Function,
        declaration: Some(ResolvedFunctionOverrideTarget::Module(
            CallableId::from_raw(7),
        )),
        interface: false,
        direct_receiver: None,
        parameters: &[],
        result: Ty::String,
        descriptor: "()Ljava/lang/String;",
    }
}

#[test]
fn the_hash_uses_the_selected_declaration_segment_and_utf16() {
    let ir = IrFile::default();
    let selected = SuperAccessors::select(&ir, &Names, request()).unwrap();
    assert_eq!(selected.qualifier_name, "Base$Part");
    assert_eq!(
        accessor_name("result", &selected.qualifier_name),
        "access$result$s131979750"
    );
    assert_eq!(accessor_name("make", "Parent"), "access$make$s-1911556918");
    // A supplementary code point contributes the two JVM UTF-16 code units.
    assert_eq!(accessor_name("read", "\u{1f600}"), "access$read$s1772899");
}

#[test]
fn missing_declaration_name_evidence_does_not_hash_an_invocation_owner() {
    let ir = IrFile::default();
    let mut missing = request();
    missing.qualifier = type_name("pkg/Unknown");
    assert_eq!(
        SuperAccessors::select(&ir, &Names, missing).err(),
        Some(ModuleRealizationTarget::Classifier(type_name(
            "pkg/Unknown"
        ))),
    );
}

#[test]
fn a_function_accessor_requires_the_selected_function_identity() {
    let ir = IrFile::default();
    let mut missing = request();
    missing.declaration = None;
    assert_eq!(
        SuperAccessors::select(&ir, &Names, missing).err(),
        Some(ModuleRealizationTarget::Classifier(type_name(
            "pkg/Scope$Base"
        ))),
    );
}

#[test]
fn reuse_is_bound_to_target_parent_qualifier_kind_and_abi() {
    let ir = IrFile::default();
    let selected = SuperAccessors::select(&ir, &Names, request()).unwrap();
    let mut cache = SuperAccessors::default();
    cache.record(selected, 3, 0);
    let repeated = SuperAccessors::select(&ir, &Names, request()).unwrap();
    assert_eq!(cache.existing(&repeated), Some(3));

    let mut different = repeated;
    different.key.target = Target::Function(ResolvedFunctionOverrideTarget::Module(
        CallableId::from_raw(8),
    ));
    assert_eq!(cache.existing(&different), None);
    let mut different = SuperAccessors::select(&ir, &Names, request()).unwrap();
    different.key.parent = type_name("pkg/OtherChild");
    assert_eq!(cache.existing(&different), None);
    let mut different = SuperAccessors::select(&ir, &Names, request()).unwrap();
    different.key.qualifier = type_name("pkg/OtherQualifier");
    assert_eq!(cache.existing(&different), None);
    let mut different = SuperAccessors::select(&ir, &Names, request()).unwrap();
    different.key.descriptor = "(I)Ljava/lang/String;".into();
    assert_eq!(cache.existing(&different), None);

    let mut getter = request();
    getter.kind = IrSuperCallKind::PropertyGetter(krusty::fir::PropertyId::from_raw(2));
    getter.declaration = None;
    let getter = SuperAccessors::select(&ir, &Names, getter).unwrap();
    cache.record(getter, 4, 1);
    let mut setter = request();
    setter.kind = IrSuperCallKind::PropertySetter(krusty::fir::PropertyId::from_raw(2));
    setter.declaration = None;
    assert_eq!(
        cache.existing(&SuperAccessors::select(&ir, &Names, setter).unwrap()),
        None
    );
}

fn bridge(ir: &mut IrFile, mapped: &str) -> (FunId, ExprId) {
    let invocation = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Special {
            owner: type_name("java/lang/Object"),
            name: mapped.to_owned(),
            descriptor: "()Ljava/lang/String;".into(),
            interface: false,
            source_member: None,
            source: Some(CallableId::from_raw(7)),
        }),
        dispatch_receiver: None,
        args: Vec::new(),
    });
    let function = ir.add_fun(IrFunction {
        name: "pending".into(),
        params: Vec::new(),
        ret: Ty::String,
        body: Some(invocation),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    (function, invocation)
}

#[test]
fn final_names_follow_the_exact_mapped_invocation() {
    let mut ir = IrFile::default();
    let selected = SuperAccessors::select(&ir, &Names, request()).unwrap();
    let (function, invocation) = bridge(&mut ir, "source");
    let mut cache = SuperAccessors::default();
    cache.record(selected, function, invocation);
    let IrExpr::Call { callee, .. } = &mut ir.exprs[invocation as usize] else {
        unreachable!();
    };
    let Callee::Special { name, .. } = &mut **callee else {
        unreachable!();
    };
    *name = "mapped-value$module".into();
    cache.finalize(&mut ir).unwrap();
    assert_eq!(
        ir.functions[function as usize].name,
        "access$mapped-value$module$s131979750"
    );
}

#[test]
fn a_lost_invocation_rejects_before_publishing_any_names() {
    let mut ir = IrFile::default();
    let selected = SuperAccessors::select(&ir, &Names, request()).unwrap();
    let (first, invocation) = bridge(&mut ir, "first");
    let mut cache = SuperAccessors::default();
    cache.record(selected, first, invocation);
    let selected = SuperAccessors::select(&ir, &Names, request()).unwrap();
    let (second, invocation) = bridge(&mut ir, "second");
    cache.record(selected, second, invocation);
    ir.exprs[invocation as usize] = IrExpr::GetValue(0);
    assert_eq!(
        cache.finalize(&mut ir),
        Err(ModuleRealizationTarget::Function(second))
    );
    assert_eq!(ir.functions[first as usize].name, "pending");
    assert_eq!(ir.functions[second as usize].name, "pending");
}
