use super::*;
use crate::backend::{DependencyExportSignatures, ExportedConstructor};
use crate::metadata::id_signature::{
    callable_signature, CallableShape, ClassScope, DeclarationContainer, KlibDeclarationSignature,
    Placement,
};
use crate::metadata::semantic::KotlinType;

fn lower(source: &str) -> IrFile {
    crate::fir_lower::tests::lower_single_source_with_jvm_stdlib(source, "CompanionConstruction")
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

fn refusal(ir: &IrFile, class: ClassId) -> Result<(), super::super::Unsupported> {
    Err(format!(
        "a class with a companion object (`{}`)",
        ir.classes[class as usize].fq_name()
    ))
}

#[test]
fn a_constructor_reference_adapter_is_an_emitted_construction() {
    let ir = lower(
        "class Holder { companion object {} }\n\
         fun factory(): () -> Holder = ::Holder\n",
    );
    let class = holder(&ir);
    assert!(requires_initialization(&ir, class));
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::CallableReference(reference)
            if matches!(&reference.target, crate::ir::IrCallableReferenceTarget::Constructor { .. })
    )));
    assert_eq!(
        super::super::check_supported(&ir, class),
        refusal(&ir, class)
    );
}

#[test]
fn a_realized_export_entry_is_checked_before_its_constructor_can_be_published() {
    let mut ir = lower("class Holder { companion object {} }\nfun box(): String = \"OK\"\n");
    let class = holder(&ir);
    assert!(!requires_initialization(&ir, class));
    assert_eq!(super::super::check_supported(&ir, class), Ok(()));
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
    let mut exports = DependencyExportSignatures::default();
    exports.record_constructor(
        ExportedConstructor {
            class,
            secondary: None,
        },
        crate::libraries::KlibDependencyCallable {
            module: "fixture".into(),
            declaration: KlibDeclarationSignature::Public(signature),
        },
    );
    crate::backend::construction_entries::realize(&mut ir, &mut exports);
    assert_eq!(
        ir.construction_entries
            .values()
            .copied()
            .collect::<Vec<_>>(),
        [class]
    );
    assert!(requires_initialization(&ir, class));
    assert_eq!(
        super::super::check_supported(&ir, class),
        refusal(&ir, class)
    );
}

#[test]
fn orphan_and_cloning_only_constructions_are_not_executable_obligations() {
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
    assert!(requires_initialization(&ir, class));
    ir.inline_fns.insert(template);
    ir.inline_only_fns.insert(template);
    assert!(!requires_initialization(&ir, class));
    assert_eq!(super::super::check_supported(&ir, class), Ok(()));
    ir.inline_fns.remove(&template);
    ir.inline_only_fns.remove(&template);
    ir.functions[template as usize].body = None;
    assert!(ir.exprs.iter().any(|expression| matches!(expression, IrExpr::New { internal, .. } if *internal == ir.classes[class as usize].fq_name)));
    assert!(!requires_initialization(&ir, class));
    assert_eq!(super::super::check_supported(&ir, class), Ok(()));
}
