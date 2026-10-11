use crate::ir::IrExpr;
use crate::types::{Ty, TypeName};

fn lower(source: &str, stem: &str) -> crate::ir::IrFile {
    stdlib::lower(source, stem)
}

#[cfg(test)]
mod stdlib {
    pub(super) fn lower(source: &str, stem: &str) -> crate::ir::IrFile {
        let platform: Box<dyn crate::libraries::SemanticPlatform> = Box::new(
            crate::jvm::jvm_libraries::JvmLibraries::new(std::rc::Rc::new(
                crate::jvm::classpath::Classpath::new(crate::toolchain::jvm_classpath_jars_for(
                    "// WITH_STDLIB",
                )),
            ))
            .expect("JVM provider initialization"),
        );
        super::super::super::tests::lower_single_source_with_platform(source, stem, platform)
    }
}

fn function_named(ir: &crate::ir::IrFile, name: &str) -> u32 {
    ir.functions
        .iter()
        .position(|function| function.name == name)
        .unwrap_or_else(|| panic!("{name}")) as u32
}

fn constructed_anonymous_classes(ir: &crate::ir::IrFile, function: u32) -> Vec<u32> {
    let body = ir.functions[function as usize]
        .body
        .unwrap_or_else(|| panic!("{function} has a body"));
    let mut found = Vec::new();
    let mut pending = vec![body];
    let mut seen = std::collections::HashSet::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let IrExpr::Lambda { .. } = ir.expr(expression) {
            continue;
        }
        if let IrExpr::New { internal, .. } = ir.expr(expression) {
            if let Some(class) = ir.class_id_by_name(*internal) {
                if ir.classes[class as usize].is_anonymous_object {
                    found.push(class);
                }
            }
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    found.sort_unstable();
    found.dedup();
    found
}

fn anonymous_type_arguments(ir: &crate::ir::IrFile, function: u32) -> Vec<TypeName> {
    let body = ir.functions[function as usize]
        .body
        .unwrap_or_else(|| panic!("{function} has a body"));
    let mut found = Vec::new();
    let mut pending = vec![body];
    let mut seen = std::collections::HashSet::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let IrExpr::Lambda { .. } = ir.expr(expression) {
            continue;
        }
        if let IrExpr::Call {
            callee: crate::ir::Callee::External { substitutions, .. },
            ..
        } = ir.expr(expression)
        {
            for substitution in substitutions {
                if let Ty::Obj(name, _) = substitution.value.non_null() {
                    if ir
                        .class_id_by_name(name)
                        .is_some_and(|class| ir.classes[class as usize].is_anonymous_object)
                    {
                        found.push(name);
                    }
                }
            }
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    found
}

const LOCAL_OBJECT: &str = "\
import kotlin.reflect.typeOf\n\
import kotlin.reflect.KType\n\
inline fun <reified T> typeOfX(x: T) = typeOf<T>()\n\
inline fun typeOfLocal(crossinline f: () -> Unit): Pair<Any, KType> {\n\
    val x = object { fun foo() = f() }\n\
    return x to typeOfX(x)\n\
}\n\
fun box(): String {\n\
    val first = typeOfLocal { 123 }\n\
    val second = typeOfLocal { 1234 }\n\
    if (first.first::class != first.second.classifier) return \"FAIL 1\"\n\
    if (second.first::class != second.second.classifier) return \"FAIL 2\"\n\
    if (first.first::class == second.first::class) return \"FAIL 3\"\n\
    return \"OK\"\n\
}\n";

#[test]
fn an_inlined_crossinline_object_is_a_distinct_class_at_each_call() {
    let ir = lower(LOCAL_OBJECT, "LocalObjectTypeOf");
    let declaration = ir
        .classes
        .iter()
        .position(|class| {
            class.is_anonymous_object
                && class.ctor_args.iter().any(|argument| {
                    argument.provenance == crate::ir::IrCtorParameterProvenance::Capture
                })
        })
        .expect("the declaration class keeps the captured lambda") as u32;
    let copies = constructed_anonymous_classes(&ir, function_named(&ir, "box"));
    assert_eq!(copies.len(), 2, "each call constructs its own object");
    assert!(copies.iter().all(|class| *class != declaration));
    assert!(copies.iter().all(|class| {
        ir.specialized_anonymous_classes.contains_key(class)
            && ir.classes[*class as usize].ctor_args.is_empty()
            && ir.classes[*class as usize].fields.is_empty()
    }));
    let arguments = anonymous_type_arguments(&ir, function_named(&ir, "box"));
    let copy_names = copies
        .iter()
        .map(|class| ir.classes[*class as usize].fq_name)
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        arguments
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>(),
        copy_names
    );
    assert!(!ir.reified_anonymous_declarations.contains(&declaration));
}
