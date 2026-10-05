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

fn construction_params(ir: &crate::ir::IrFile, function: u32) -> Vec<Ty> {
    let body = ir.functions[function as usize]
        .body
        .unwrap_or_else(|| panic!("{function} has a body"));
    let mut pending = vec![body];
    let mut seen = std::collections::HashSet::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let IrExpr::New {
            internal,
            ctor_params,
            ..
        } = ir.expr(expression)
        {
            if let Some(class) = ir.class_id_by_name(*internal) {
                if ir.classes[class as usize].is_anonymous_object {
                    return ctor_params.clone().unwrap_or_default();
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
    assert!(ir.reified_anonymous_declarations.contains(&declaration));
    assert!(!ir.reified_anonymous_declarations.contains(&call));
}

const REIFIED_PROPERTIES: &str = "\
interface Cell<T> {\n\
    val item: T\n\
    var slot: T\n\
    fun shown(): String\n\
}\n\
class Token\n\
inline fun <reified T : Any> make(seed: T): Cell<T> = object : Cell<T> {\n\
    override val item: T\n\
        get() = if (T::class.simpleName != null) seed else seed\n\
    override var slot: T = seed\n\
        get() = field\n\
        set(value) {\n\
            val probe: Any = value\n\
            if (probe is T) field = value\n\
        }\n\
    val T.mark: Boolean\n\
        get() = T::class.simpleName != null\n\
    override fun shown(): String = if (item.mark) \"y\" else \"n\"\n\
}\n\
fun box(): String = make(Token()).shown()\n";

fn property_named(ir: &crate::ir::IrFile, class: u32, name: &str) -> (Option<u32>, Option<u32>) {
    let property = ir.classes[class as usize]
        .properties
        .iter()
        .find(|property| property.name == name)
        .unwrap_or_else(|| panic!("{name} on class {class}"));
    (property.getter, property.setter)
}

fn override_named(ir: &crate::ir::IrFile, class: u32, name: &str) -> crate::ir::IrPropertyOverride {
    let owner = ir.classes[class as usize].fq_name;
    ir.property_overrides
        .get(&owner)
        .and_then(|edges| edges.iter().find(|edge| edge.name == name))
        .cloned()
        .unwrap_or_else(|| panic!("override {name} on {class}"))
}

#[test]
fn a_reified_anonymous_object_copies_its_property_accessors() {
    let ir = lower(REIFIED_PROPERTIES, "ReifiedAnonymousProperties");
    let declaration = ir
        .classes
        .iter()
        .enumerate()
        .find_map(|(index, class)| {
            let index = u32::try_from(index).expect("class index");
            (class.is_anonymous_object && ir.reified_anonymous_declarations.contains(&index))
                .then_some(index)
        })
        .expect("the declaration class is recorded for reification");
    let call = constructed_class(&ir, function_named(&ir, "box"));
    assert_ne!(declaration, call);
    assert!(!ir.reified_anonymous_declarations.contains(&call));
    let construction = construction_params(&ir, function_named(&ir, "box"));
    assert_eq!(
        construction,
        ir.classes[call as usize]
            .ctor_args
            .iter()
            .map(|argument| argument.ty)
            .collect::<Vec<_>>()
    );
    assert!(construction
        .iter()
        .all(|ty| ty.non_null() != Ty::obj("Token")));

    for name in ["item", "slot"] {
        let (source_getter, source_setter) = property_named(&ir, declaration, name);
        let (copied_getter, copied_setter) = property_named(&ir, call, name);
        let getter = copied_getter.expect(name);
        assert_ne!(Some(getter), source_getter);
        assert_eq!(
            ir.functions[getter as usize].ret,
            ir.functions[source_getter.expect(name) as usize].ret
        );
        assert!(ir.classes[call as usize].methods.contains(&getter));
        assert!(!ir.classes[declaration as usize].methods.contains(&getter));
        let edge = override_named(&ir, call, name);
        assert_eq!(edge.implementation_owner, ir.classes[call as usize].fq_name);
        assert_eq!(edge.implementation_getter, Some(getter));
        if name == "slot" {
            let setter = copied_setter.expect("slot setter");
            assert_ne!(Some(setter), source_setter);
            assert!(ir.classes[call as usize].methods.contains(&setter));
            assert_eq!(edge.implementation_setter, Some(setter));
        } else {
            assert!(copied_setter.is_none());
            assert!(edge.implementation_setter.is_none());
        }
    }

    let source_name = ir.classes[declaration as usize].fq_name;
    let copy_name = ir.classes[call as usize].fq_name;
    let source_mark = ir
        .member_ext_props
        .get(&source_name)
        .and_then(|properties| properties.iter().find(|property| property.name == "mark"))
        .expect("the declaration publishes the member extension");
    let copied_mark = ir
        .member_ext_props
        .get(&copy_name)
        .and_then(|properties| properties.iter().find(|property| property.name == "mark"))
        .expect("the copy publishes the member extension");
    assert_ne!(copied_mark.getter, source_mark.getter);
    assert!(ir.classes[call as usize]
        .methods
        .contains(&copied_mark.getter));
    assert_eq!(copied_mark.receiver, source_mark.receiver);
    assert_eq!(
        ir.functions[copied_mark.getter as usize].ret,
        ir.functions[source_mark.getter as usize].ret
    );

    let shown = ir.classes[call as usize]
        .methods
        .iter()
        .copied()
        .find(|method| ir.functions[*method as usize].name == "shown")
        .expect("shown");
    let calls = method_calls(&ir, shown);
    assert!(
        calls.len() >= 2
            && calls.iter().all(|method| {
                ir.classes[call as usize].methods.contains(method)
                    && !ir.classes[declaration as usize].methods.contains(method)
            }),
        "shown calls the copy's accessors, got {calls:?}"
    );
}

const TYPE_OF_PROPERTY: &str = "\
import kotlin.reflect.typeOf\n\
class Token\n\
inline fun <reified T> foo(): Any = object { val x = typeOf<T>() }.x\n\
fun box(): Any = foo<Token>()\n";

const NESTED_TYPE_OF_PROPERTY: &str = "\
import kotlin.reflect.typeOf\n\
class Token\n\
inline fun <reified T> foo(): Any = object { val x = typeOf<T>() }.x\n\
inline fun <reified T> bar(): Any = foo<List<T>>()\n\
fun box(): Any = bar<Token>()\n";

fn reified_substitution_values(ir: &crate::ir::IrFile, class: u32) -> Vec<Ty> {
    let mut roots = ir.classes[class as usize]
        .methods
        .iter()
        .filter_map(|method| ir.functions[*method as usize].body)
        .collect::<Vec<_>>();
    roots.extend(ir.classes[class as usize].init_body);
    roots.extend(
        ir.classes[class as usize]
            .properties
            .iter()
            .filter_map(|property| property.initializer),
    );
    let mut pending = roots;
    let mut seen = std::collections::HashSet::new();
    let mut values = Vec::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let IrExpr::Call {
            callee: crate::ir::Callee::External { substitutions, .. },
            ..
        } = ir.expr(expression)
        {
            values.extend(
                substitutions
                    .iter()
                    .filter(|substitution| substitution.reified)
                    .map(|substitution| substitution.value),
            );
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    values
}

fn mentions_type_parameter(ty: Ty) -> bool {
    match ty.non_null() {
        Ty::TyParam(..) => true,
        other => other
            .type_args()
            .iter()
            .copied()
            .any(mentions_type_parameter),
    }
}

#[test]
fn an_inlined_reified_anonymous_object_specializes_type_of_in_a_property() {
    let ir = lower(TYPE_OF_PROPERTY, "ReifiedAnonymousTypeOf");
    let declaration = ir
        .classes
        .iter()
        .enumerate()
        .find_map(|(index, class)| {
            let index = u32::try_from(index).expect("class index");
            (class.is_anonymous_object
                && reified_substitution_values(&ir, index)
                    .iter()
                    .copied()
                    .any(mentions_type_parameter))
            .then_some(index)
        })
        .expect("the declaration class keeps typeOf<T>()");
    let call = constructed_class(&ir, function_named(&ir, "box"));
    assert_ne!(declaration, call);
    let call_values = reified_substitution_values(&ir, call);
    assert!(
        call_values
            .iter()
            .any(|ty| ty.non_null() == Ty::obj("Token")),
        "the call-site typeOf argument is Token, got {call_values:?}"
    );
    assert!(
        call_values
            .iter()
            .copied()
            .all(|ty| !mentions_type_parameter(ty)),
        "the call-site typeOf argument is not left as a type parameter, got {call_values:?}"
    );
    assert!(ir.reified_anonymous_declarations.contains(&declaration));
    assert!(!ir.reified_anonymous_declarations.contains(&call));
}

#[test]
fn a_nested_inline_specializes_type_of_inside_the_anonymous_object() {
    let ir = lower(NESTED_TYPE_OF_PROPERTY, "NestedReifiedAnonymousTypeOf");
    let call = constructed_class(&ir, function_named(&ir, "box"));
    let call_values = reified_substitution_values(&ir, call);
    let list_of_token = Ty::obj_args("kotlin/collections/List", &[Ty::obj("Token")]);
    assert!(
        call_values.iter().any(|ty| ty.non_null() == list_of_token),
        "the outer call specializes typeOf to List<Token>, got {call_values:?}"
    );
    assert!(
        call_values
            .iter()
            .copied()
            .all(|ty| !mentions_type_parameter(ty)),
        "the outer call does not leave a type parameter in typeOf, got {call_values:?}"
    );
}

fn method_calls(ir: &crate::ir::IrFile, function: u32) -> Vec<u32> {
    let Some(body) = ir.functions[function as usize].body else {
        return Vec::new();
    };
    let mut pending = vec![body];
    let mut seen = std::collections::HashSet::new();
    let mut calls = Vec::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let IrExpr::MethodCall { class, index, .. } = ir.expr(expression) {
            if let Some(method) = ir.classes[*class as usize].methods.get(*index as usize) {
                calls.push(*method);
            }
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    calls
}
