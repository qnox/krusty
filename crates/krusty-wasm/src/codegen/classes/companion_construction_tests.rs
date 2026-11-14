//! A companion holder's constructors initialize its companion, so every construction path
//! (a constructor reference adapter, a published export entry, or an inline template) is
//! supported.

use krusty::ir::{ClassId, IrExpr, IrFile};
use krusty::metadata::id_signature::{
    callable_signature, CallableShape, ClassScope, DeclarationContainer, KlibDeclarationSignature,
    Placement,
};
use krusty::metadata::semantic::KotlinType;

fn lower(source: &str) -> IrFile {
    krusty::fir_lower::single_source_lowering::lower_single_source_with_jvm_stdlib(
        source,
        "CompanionConstruction",
    )
}

fn holder(ir: &IrFile) -> ClassId {
    let classes = ir
        .classes
        .iter()
        .enumerate()
        .filter_map(|(class, declaration)| declaration.companion_class.map(|_| class as ClassId))
        .collect::<Vec<_>>();
    let [class] = classes.as_slice() else {
        panic!("one declared companion holder")
    };
    *class
}

#[test]
fn a_companion_holder_constructed_through_a_reference_adapter_is_supported() {
    let ir = lower(
        "class Holder { companion object {} }\n\
         fun factory(): () -> Holder = ::Holder\n",
    );
    let class = holder(&ir);
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::CallableReference(reference)
            if matches!(&reference.target, krusty::ir::IrCallableReferenceTarget::Constructor { .. })
    )));
    assert_eq!(super::check_supported(&ir, class), Ok(()));
}

#[test]
fn a_companion_holder_with_a_realized_export_entry_is_supported() {
    let mut ir = lower("class Holder { companion object {} }\nfun box(): String = \"OK\"\n");
    let class = holder(&ir);
    assert_eq!(super::check_supported(&ir, class), Ok(()));
    let signature = callable_signature(
        DeclarationContainer::<KotlinType> {
            package: &[],
            classes: &[ClassScope {
                name: "Holder",
                type_parameters: Vec::new(),
                expect: false,
            }],
            native_interop_library: false,
        },
        &CallableShape {
            name: "<init>",
            contexts: Vec::new(),
            receiver: None,
            params: Vec::new(),
            vararg: None,
            type_parameters: Vec::new(),
            expect: false,
            placement: Placement::Ordinary,
        },
    )
    .expect("the source no-argument constructor signature");
    krusty::backend::construction_entries::realize_primary_constructor_for_test(
        &mut ir,
        class,
        krusty::libraries::KlibDependencyCallable {
            module: "fixture".into(),
            declaration: KlibDeclarationSignature::Public(signature),
        },
    );
    // The constructor's entry, and the instance entry every object and companion has.
    let companion = ir
        .class_id_by_name(
            ir.classes[class as usize]
                .companion_class
                .expect("the holder's companion"),
        )
        .expect("the companion's class");
    let mut entries = ir
        .construction_entries()
        .values()
        .copied()
        .collect::<Vec<_>>();
    entries.sort_unstable();
    let mut expected = [class, companion];
    expected.sort_unstable();
    assert_eq!(entries, expected);
    assert_eq!(super::check_supported(&ir, class), Ok(()));
}

#[test]
fn a_companion_holder_constructed_only_by_templates_is_supported() {
    let mut ir = lower(
        "class Holder { companion object {} }\n\
         fun template(): Holder = Holder()\nfun box(): String = \"OK\"\n",
    );
    let class = holder(&ir);
    let template = ir
        .functions
        .iter()
        .position(|function| function.name == "template")
        .expect("the source template") as u32;
    ir.inline_fns.insert(template);
    ir.inline_only_fns.insert(template);
    assert_eq!(super::check_supported(&ir, class), Ok(()));
    ir.inline_fns.remove(&template);
    ir.inline_only_fns.remove(&template);
    ir.functions[template as usize].body = None;
    assert!(ir.exprs.iter().any(|expression| matches!(expression, IrExpr::New { internal, .. } if *internal == ir.classes[class as usize].fq_name)));
    assert_eq!(super::check_supported(&ir, class), Ok(()));
}
