use crate::ir::IrExpr;
use crate::types::Ty;

fn lower(source: &str, stem: &str) -> crate::ir::IrFile {
    stdlib::lower(source, stem)
}

#[cfg(test)]
mod stdlib {
    pub(super) fn lower(source: &str, stem: &str) -> crate::ir::IrFile {
        let platform: Box<dyn crate::libraries::SemanticPlatform> = Box::new(
            crate::jvm::jvm_libraries::JvmLibraries::new(std::rc::Rc::new(
                crate::jvm::classpath::Classpath::new(crate::toolchain::classpath_jars_for(
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

fn class_literals(ir: &crate::ir::IrFile, class: u32) -> Vec<Ty> {
    let mut literals = Vec::new();
    for method in &ir.classes[class as usize].methods {
        let Some(body) = ir.functions[*method as usize].body else {
            continue;
        };
        let mut pending = vec![body];
        let mut seen = std::collections::HashSet::new();
        while let Some(expression) = pending.pop() {
            if !seen.insert(expression) {
                continue;
            }
            if let IrExpr::KClassLiteral {
                classifier: Some(classifier),
                ..
            } = ir.expr(expression)
            {
                literals.push(*classifier);
            }
            crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
        }
    }
    literals
}

fn constructed_class(ir: &crate::ir::IrFile, function: u32) -> u32 {
    let body = ir.functions[function as usize]
        .body
        .unwrap_or_else(|| panic!("{function} has a body"));
    let mut pending = vec![body];
    let mut seen = std::collections::HashSet::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let IrExpr::New { internal, .. } = ir.expr(expression) {
            if let Some(class) = ir.class_id_by_name(*internal) {
                if ir.classes[class as usize].is_anonymous_object {
                    return class;
                }
            }
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    panic!("function {function} constructs an anonymous object")
}

const REIFIED_OBJECT: &str = "\
interface Face { fun foo(): String? }\n\
class Token\n\
inline fun <reified T> a(): Face = object : Face {\n\
    override fun foo(): String? = T::class.simpleName\n\
}\n\
fun box(): String? = a<Token>().foo()\n";

#[test]
fn an_inlined_reified_anonymous_object_specializes_only_the_call_site_class() {
    let ir = lower(REIFIED_OBJECT, "ReifiedAnonymousObject");
    let declaration = ir
        .classes
        .iter()
        .enumerate()
        .find_map(|(index, class)| {
            let index = u32::try_from(index).expect("class index");
            (class.is_anonymous_object
                && class_literals(&ir, index)
                    .iter()
                    .any(|ty| matches!(ty.non_null(), Ty::TyParam(..))))
            .then_some(index)
        })
        .expect("the declaration class keeps the reified parameter");
    let call = constructed_class(&ir, function_named(&ir, "box"));
    assert_ne!(declaration, call);
    assert!(
        class_literals(&ir, call)
            .iter()
            .any(|ty| ty.non_null() == Ty::obj("Token")),
        "the call-site class uses the reified argument, got {:?}",
        class_literals(&ir, call)
    );
    assert!(
        !class_literals(&ir, call)
            .iter()
            .any(|ty| matches!(ty.non_null(), Ty::TyParam(..))),
        "the call-site class does not keep the reified parameter"
    );
}
