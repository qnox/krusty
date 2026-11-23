//! The class names kotlinc-wasm gives function values, which `kotlin.Any.toString` prints.
//!
//! kotlinc-wasm builds a lambda or a callable reference as an instance of a class named for its
//! shape, not for its source. The name is the function type's family and arity, then `R` for a
//! reference, then `_bound`, the count of captured values and one code per captured value when it
//! captures any: `Function0`, `SuspendFunction1_bound1_I`, `KFunction1R`,
//! `KFunction0R_bound1_A`. `toString` prints that simple name, `@` and the identity hash, as for
//! any class (`super::runtime::object_strings`). The names were measured with kotlinc-wasm 2.4.21,
//! wasm-js and wasm-wasi alike: `wasm_function_values_print_their_class_names` in
//! `tests/kotlin_box_wasm_conformance.rs` pins them.
//!
//! A lambda converted to a `fun interface` is an instance of a class named for the interface and
//! for where the code holding the conversion stands ([`sam_class_name`]). A reference to a property
//! is not given a name here: it prints the property instead ([`property_reference_text`]).

use krusty::backend::class_names::{member_class_names, site_container_name};
use krusty::backend::{BackendClassifierSource, CheckedBackendCallables};
use krusty::ir::{
    ExprId, IrCallableReference, IrCallableReferenceTarget, IrExpr, IrFile, IrSamPlacement,
};
use krusty::types::{FnSig, Ty, TyKind};

/// What a function value's class name is derived from, apart from its captures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct NamedShape {
    /// A callable reference, whose family is `KFunction`; otherwise a lambda's `Function`.
    pub(super) reference: bool,
    /// A `suspend` function type, whose family carries the `Suspend` prefix.
    pub(super) suspend: bool,
    /// The function type's parameters, a receiver and context parameters included.
    pub(super) arity: usize,
}

/// The shape kotlinc-wasm names the function value `expression` creates, a lambda of the checked
/// type `instance` or a callable reference, or `None` for a value it names otherwise.
pub(super) fn named_shape(expression: &IrExpr, instance: Option<Ty>) -> Option<NamedShape> {
    let (reference, function_type) = match expression {
        IrExpr::Lambda { sam: None, .. } => (false, instance?),
        IrExpr::CallableReference(reference)
            if !matches!(reference.target, IrCallableReferenceTarget::Property(_)) =>
        {
            (true, reference.function_type)
        }
        _ => return None,
    };
    let signature = function_signature(function_type)?;
    Some(NamedShape {
        reference,
        suspend: signature.suspend,
        arity: signature.params.len(),
    })
}

fn is_reflective_property_reference(reference: &IrCallableReference) -> bool {
    matches!(&reference.target, IrCallableReferenceTarget::Property(property) if property.reflective)
}

/// What `toString` of the `KProperty` value `reference` creates prints: kotlinc-wasm's runtime has
/// no reflection, so it prints `property <name>` and says so, bound or not, `val` or `var`, on
/// wasm-js and wasm-wasi alike. `None` when the property's name cannot be read or is not ASCII,
/// which the constant rendering holds.
pub(super) fn property_reference_text(
    ir: &IrFile,
    callables: &CheckedBackendCallables,
    reference: &IrCallableReference,
) -> Option<String> {
    if !is_reflective_property_reference(reference) {
        return None;
    }
    let name = krusty::backend::reference_names::callable_reference_name(ir, callables, reference)?;
    name.is_ascii()
        .then(|| format!("property {name} (Kotlin reflection is not available)"))
}

/// The qualified name of the class kotlinc-wasm compiles the conversion `expression` of a lambda
/// to a `fun interface` to, or `None` for any other expression and for a conversion whose
/// container it names otherwise.
///
/// The name is the container's qualified name, then `sam$`, the interface's qualified name with
/// `_` for `.`, and `$0`: `p.q.C.sam$p_q_F$0`. The container is the member classifier whose
/// member holds the conversion, or else the package of the file holding it, whose root adds no
/// segment (`sam$kotlin_Comparator$0`). Inlined code stands in its caller. kotlinc-wasm 2.5.0-RC
/// keeps one class per container and interface, so every such conversion shares the `$0` name.
/// `wasm_sam_conversions_print_their_class_names` in `tests/kotlin_box_wasm_conformance.rs` pins
/// them.
pub(super) fn sam_class_name(ir: &IrFile, expression: ExprId) -> Option<String> {
    let IrExpr::Lambda {
        sam: Some(target), ..
    } = ir.expr(expression)
    else {
        return None;
    };
    // Boundary conversions: kotlinc-wasm spells these names into the class name it prints.
    let mut name = match target.placement {
        IrSamPlacement::ThisFile => match ir.expression_owners.get(&expression) {
            Some(&owner) => member_classifier_name(ir, owner)?,
            None => ir.package.clone().unwrap_or_default(),
        },
        IrSamPlacement::Dependency(container) => site_container_name(ir, container)?,
        IrSamPlacement::UnnamedDependencyClass => return None,
    };
    if !name.is_empty() {
        name.push('.');
    }
    name.push_str("sam$");
    name.push_str(
        &member_class_names(target.classifier)
            .qualified
            .replace('.', "_"),
    );
    name.push_str("$0");
    Some(name)
}

/// The qualified name of this file's member classifier `owner`. A local classifier or an
/// anonymous object has none of this shape.
fn member_classifier_name(ir: &IrFile, owner: krusty::types::TypeName) -> Option<String> {
    let class = ir.class_id_by_name(owner)?;
    let declaration = &ir.classes[class as usize];
    if declaration.is_local_class
        || declaration.is_anonymous_object
        || ir.local_class_name_provenance.contains_key(&class)
    {
        return None;
    }
    Some(krusty::backend::class_names::source_class_names(ir, class)?.qualified)
}

fn function_signature(ty: Ty) -> Option<&'static FnSig> {
    match ty.non_null().kind() {
        TyKind::Fun(signature) => Some(signature),
        _ => None,
    }
}

/// How kotlinc-wasm encodes one captured value in a class name, by the type its capture keeps
/// after erasure ([`erased_capture`]): a primitive or an unsigned number by its own code, a value
/// class that erases to one by `V` and its qualified name with `_` for `.`, and anything else, a
/// nullable value and a shared `var` cell included, as the reference `A`.
pub(super) fn capture_code(
    ty: Ty,
    shared: bool,
    classifiers: &dyn BackendClassifierSource,
) -> String {
    if shared {
        return "A".to_owned();
    }
    let Some(kept) = erased_capture(ty, classifiers) else {
        return "A".to_owned();
    };
    let code = match kept.kind() {
        TyKind::INT => "I",
        TyKind::LONG => "J",
        TyKind::DOUBLE => "D",
        TyKind::FLOAT => "F",
        TyKind::BOOLEAN => "Z",
        TyKind::CHAR => "C",
        TyKind::SHORT => "S",
        TyKind::BYTE => "B",
        TyKind::UBYTE => "UB",
        TyKind::USHORT => "US",
        TyKind::UINT => "UI",
        TyKind::ULONG => "UJ",
        TyKind::Obj(class, _) => {
            // Boundary conversion: the class name kotlinc-wasm spells into the generated one.
            let qualified = member_class_names(class).qualified;
            return format!("V{}", qualified.replace('.', "_"));
        }
        _ => "A",
    };
    code.to_owned()
}

/// The type a captured value of type `ty` keeps in kotlinc-wasm's class name, or `None` when it
/// is erased to a reference. A primitive and an unsigned number keep their type, a type parameter
/// erases as its first bound does, and a non-null value class keeps its own type only when its
/// underlying type erases to a primitive or an unsigned number. A nullable value is boxed, so it
/// is a reference.
fn erased_capture(ty: Ty, classifiers: &dyn BackendClassifierSource) -> Option<Ty> {
    if ty.is_builtin_scalar() {
        return Some(ty);
    }
    match ty.kind() {
        TyKind::TyParam(_, bound) => erased_capture(bound, classifiers),
        TyKind::Obj(class, _) => {
            let underlying = classifiers.classifier(class)?.value_underlying?;
            let erased = erased_capture(underlying, classifiers)?;
            erased.is_builtin_scalar().then_some(ty)
        }
        _ => None,
    }
}

/// The simple class name of a function value of `shape` capturing values encoded as `captures`.
pub(super) fn class_name(shape: NamedShape, captures: &[String]) -> String {
    let mut name = String::new();
    if shape.reference {
        name.push('K');
    }
    if shape.suspend {
        name.push_str("Suspend");
    }
    name.push_str("Function");
    name.push_str(&shape.arity.to_string());
    if shape.reference {
        name.push('R');
    }
    if !captures.is_empty() {
        name.push_str("_bound");
        name.push_str(&captures.len().to_string());
        name.push('_');
        name.extend(captures.iter().map(String::as_str));
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;
    use krusty::backend::BackendClassifierFact;
    use krusty::types::{type_name, type_name_nested_child, TypeName};
    use std::sync::Arc;

    /// The classifier facts of value classes, each by its underlying type.
    struct ValueClasses(Vec<(TypeName, Ty)>);

    impl BackendClassifierSource for ValueClasses {
        fn classifier(&self, name: TypeName) -> Option<Arc<BackendClassifierFact>> {
            let &(_, underlying) = self.0.iter().find(|(class, _)| *class == name)?;
            Some(Arc::new(BackendClassifierFact {
                klib_identity: None,
                runtime_exception: None,
                declaration_name: None,
                access: krusty::libraries::ClassifierAccess::Public,
                is_kotlin: true,
                mapped_builtin: false,
                source: true,
                outer_instance: None,
                kind: krusty::libraries::TypeKind::Class,
                is_abstract: false,
                is_extensible: false,
                supertypes: Box::new([]),
                annotations: Box::new([]),
                own_type_parameter_count: 0,
                type_param_variances: Box::new([]),
                value_underlying: Some(underlying),
                value_declaration: None,
                role: None,
                companion: None,
                enum_entries_accessor: None,
            }))
        }
    }

    fn shape(reference: bool, suspend: bool, arity: usize) -> NamedShape {
        NamedShape {
            reference,
            suspend,
            arity,
        }
    }

    fn codes(codes: &[&str]) -> Vec<String> {
        codes.iter().map(|code| (*code).to_owned()).collect()
    }

    /// kotlinc-wasm 2.4.21's names for these shapes, measured on wasm-js and wasm-wasi.
    #[test]
    fn names_follow_kotlinc_wasm() {
        assert_eq!(class_name(shape(false, false, 0), &[]), "Function0");
        assert_eq!(
            class_name(shape(false, false, 2), &codes(&["I"])),
            "Function2_bound1_I"
        );
        assert_eq!(
            class_name(shape(false, false, 0), &codes(&["I", "J", "A"])),
            "Function0_bound3_IJA"
        );
        assert_eq!(
            class_name(shape(false, true, 1), &codes(&["I"])),
            "SuspendFunction1_bound1_I"
        );
        assert_eq!(class_name(shape(true, false, 1), &[]), "KFunction1R");
        assert_eq!(
            class_name(shape(true, false, 0), &codes(&["A"])),
            "KFunction0R_bound1_A"
        );
        assert_eq!(class_name(shape(true, true, 0), &[]), "KSuspendFunction0R");
    }

    #[test]
    fn captures_are_coded_by_their_declared_type() {
        let none = ValueClasses(Vec::new());
        let codes = [
            Ty::Int,
            Ty::Long,
            Ty::Double,
            Ty::Float,
            Ty::Boolean,
            Ty::Char,
            Ty::Short,
            Ty::Byte,
            Ty::UByte,
            Ty::UShort,
            Ty::UInt,
            Ty::ULong,
            Ty::String,
        ]
        .map(|ty| capture_code(ty, false, &none));
        assert_eq!(
            codes,
            ["I", "J", "D", "F", "Z", "C", "S", "B", "UB", "US", "UI", "UJ", "A"]
        );
        assert_eq!(capture_code(Ty::Int, true, &none), "A");
        assert_eq!(capture_code(Ty::nullable(Ty::Int), false, &none), "A");
    }

    /// A value class keeps its identity when its underlying type erases to a primitive or an
    /// unsigned number, and is a reference otherwise: over a reference, over another value class,
    /// nullable, or shared. kotlinc-wasm 2.4.21 names `p.Outer.Inner` over `Long` `Vp_Outer_Inner`.
    #[test]
    fn value_class_captures_keep_their_qualified_name() {
        let id = type_name("p/Id");
        let inner = type_name_nested_child(type_name("p/Outer"), "Inner");
        let wrapped = type_name("p/Wrapped");
        let text = type_name("p/Text");
        let classes = ValueClasses(vec![
            (id, Ty::Int),
            (inner, Ty::Long),
            (wrapped, Ty::obj_name(id)),
            (text, Ty::String),
        ]);
        assert_eq!(capture_code(Ty::obj_name(id), false, &classes), "Vp_Id");
        assert_eq!(
            capture_code(Ty::obj_name(inner), false, &classes),
            "Vp_Outer_Inner"
        );
        assert_eq!(capture_code(Ty::obj_name(wrapped), false, &classes), "A");
        assert_eq!(capture_code(Ty::obj_name(text), false, &classes), "A");
        assert_eq!(
            capture_code(Ty::nullable(Ty::obj_name(id)), false, &classes),
            "A"
        );
        assert_eq!(capture_code(Ty::obj_name(id), true, &classes), "A");
    }
}
