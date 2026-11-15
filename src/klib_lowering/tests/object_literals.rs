//! The prepared `Continuation` factory is an object literal. Its template is the class checked
//! FIR would build for the same inline function: an anonymous object, one constructor field per
//! captured parameter, and members that read those fields.

use super::*;
use crate::compilation_target::CompilationTarget;
use crate::ir::IrExpr;
use crate::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};
use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::toolchain::kotlin_stdlib_klib;
use crate::types::type_name;

#[test]
fn continuation_factory_lowers_as_an_anonymous_object() {
    let path = kotlin_stdlib_klib(CompilationTarget::WasmJs).expect("wasm stdlib");
    let libraries = KlibLibraries::open(&[KlibLibraryInput::platform_stdlib(
        path,
        KlibStdlibSemantics::Wasm,
    )])
    .expect("open wasm stdlib");
    let symbols = libraries.symbols(
        SymbolNamespace::Package(type_name("kotlin/coroutines")),
        "Continuation",
    );
    let (functions, _) = symbols.callables.clone().into_parts();
    let function = functions
        .overloads
        .iter()
        .find(|function| {
            !function.callable.suspend
                && function.callable.params.len() == 2
                && function.flags.inline == crate::libraries::InlineKind::MustInline
        })
        .expect("the Continuation factory");
    let id = function
        .callable
        .external_identity
        .expect("the factory has a provider identity");
    let realization = libraries.external_callable(id).expect("realization");
    let hierarchy = checked_template_hierarchy(&libraries, &realization);
    let mut ir = crate::ir::IrFile::default();
    let template = lower_inline_template(
        &mut ir,
        &realization,
        libraries.inline_declaration_bodies(),
        TemplateCallees {
            declarations: libraries.declaration_bodies(),
            references: libraries.reference_bindings(),
            source: &libraries,
            type_hierarchy: &hierarchy,
        },
    )
    .expect("a klib declaration")
    .expect("the Continuation template lowers");
    let class = ir
        .classes
        .iter()
        .find(|class| class.is_anonymous_object)
        .expect("the factory builds an anonymous object");
    assert!(class.is_local_class);
    assert_eq!(class.constructor_prefix_count, 2, "{class:?}");
    assert_eq!(class.fields.len(), 2);
    assert!(class.ctor_args.iter().all(|argument| {
        argument.provenance == crate::ir::IrCtorParameterProvenance::Capture && argument.is_field
    }));
    assert!(class
        .properties
        .iter()
        .any(|property| property.name == "context" && property.getter.is_some()));
    assert!(class.methods.iter().any(|method| {
        ir.functions[*method as usize].name == "resumeWith"
            && ir.functions[*method as usize].body.is_some()
    }));
    assert!(ir
        .function_overrides
        .get(&class.fq_name_id())
        .is_some_and(|edges| !edges.is_empty()));
    assert!(ir
        .property_overrides
        .get(&class.fq_name_id())
        .is_some_and(|edges| !edges.is_empty()));
    let body = ir.functions[template.function as usize]
        .body
        .expect("the template has a body");
    assert!(
        contains_new(&ir, body),
        "the template constructs the object"
    );
}

fn contains_new(ir: &crate::ir::IrFile, expression: crate::ir::ExprId) -> bool {
    let mut pending = vec![expression];
    let mut seen = crate::fx_hash::HashSet::default();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if matches!(ir.expr(expression), IrExpr::New { .. }) {
            return true;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    false
}
