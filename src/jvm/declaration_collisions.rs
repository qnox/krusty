//! JVM declaration-signature validation after representation selection.
//!
//! Kotlin declarations can be distinct at the source level while their JVM realizations collide.
//! This pass runs only after representation passes have selected physical method names and types,
//! so FIR and common lowering never depend on JVM descriptors.

use crate::ir::{FunId, IrFile};

pub(super) fn validate(ir: &IrFile) -> Result<(), Vec<(crate::diag::Span, String)>> {
    for class in &ir.classes {
        reject_duplicate_constructors(class)
            .map_err(|message| vec![(crate::diag::Span::new(0, 0), message)])?;
        let Some(properties) = ir.member_ext_props.get(&class.fq_name) else {
            continue;
        };
        for property in properties {
            for accessor in std::iter::once(property.getter).chain(property.setter) {
                reject_duplicate_method(ir, class, accessor)
                    .map_err(|message| vec![(crate::diag::Span::new(0, 0), message)])?;
            }
        }
    }
    let clashes = package_function_signature_clashes(ir);
    if clashes.is_empty() {
        Ok(())
    } else {
        Err(clashes)
    }
}

fn reject_duplicate_constructors(class: &crate::ir::IrClass) -> Result<(), String> {
    let mut descriptors = std::collections::HashSet::new();
    if class.has_primary_ctor {
        descriptors.insert(crate::jvm::names::method_descriptor(
            &crate::jvm::ir_emit::class_ctor_jvm_tys(class),
            crate::types::Ty::Unit,
        ));
    }
    for constructor in &class.secondary_ctors {
        let parameters = crate::jvm::ir_emit::jvm_tys(&constructor.prefix_params)
            .into_iter()
            .chain(crate::jvm::ir_emit::jvm_tys(&constructor.params))
            .collect::<Vec<_>>();
        let descriptor = crate::jvm::names::method_descriptor(&parameters, crate::types::Ty::Unit);
        if !descriptors.insert(descriptor.clone()) {
            return Err(format!(
                "platform declaration clash: '{}' contains duplicate JVM constructor <init>{descriptor}",
                class.fq_name
            ));
        }
    }
    Ok(())
}

fn reject_duplicate_method(
    ir: &IrFile,
    class: &crate::ir::IrClass,
    accessor: FunId,
) -> Result<(), String> {
    let Some(accessor_function) = ir.functions.get(accessor as usize) else {
        return Err("internal error: member extension accessor has no JVM function".to_string());
    };
    let descriptor =
        crate::jvm::names::method_descriptor(&accessor_function.params, accessor_function.ret);
    if class.methods.iter().copied().any(|candidate| {
        candidate != accessor
            && ir
                .functions
                .get(candidate as usize)
                .is_some_and(|function| {
                    function.name == accessor_function.name
                        && crate::jvm::names::method_descriptor(&function.params, function.ret)
                            == descriptor
                })
    }) {
        return Err(format!(
            "platform declaration clash: '{}' contains duplicate JVM method {}{}",
            class.fq_name, accessor_function.name, descriptor
        ));
    }
    Ok(())
}

/// Package functions whose representation passes selected the same JVM method.
///
/// Different Kotlin shapes (`fun <T> id(): Int` and `fun id(): Int`, or `List<T>` and
/// `List<String>`) stay distinct in the resolver. They collide only once the physical name and
/// descriptor are known, and only inside one file facade.
fn package_function_signature_clashes(ir: &IrFile) -> Vec<(crate::diag::Span, String)> {
    let mut groups: Vec<((String, String), Vec<usize>)> = Vec::new();
    for (index, declaration) in ir.package_functions.iter().enumerate() {
        if declaration.companion {
            continue;
        }
        let Some(function) = ir.functions.get(declaration.function as usize) else {
            continue;
        };
        let signature = (
            function.name.clone(),
            crate::jvm::names::method_descriptor(&function.params, function.ret),
        );
        if let Some((_, members)) = groups.iter_mut().find(|(key, _)| *key == signature) {
            members.push(index);
        } else {
            groups.push((signature, vec![index]));
        }
    }
    let mut clashes = Vec::new();
    for ((name, descriptor), members) in groups {
        if members.len() < 2 {
            continue;
        }
        let mut rendered = members
            .iter()
            .map(|&index| &ir.package_functions[index])
            .collect::<Vec<_>>();
        rendered.sort_by(|left, right| declaration_order(left).cmp(&declaration_order(right)));
        let listed = rendered
            .iter()
            .map(|declaration| render_declaration(declaration, ir.package.as_deref()))
            .collect::<Vec<_>>();
        let message = clash_message(&name, &descriptor, &listed);
        let mut located = members
            .into_iter()
            .map(|index| (ir.package_functions[index].signature_span, message.clone()))
            .collect::<Vec<_>>();
        located.sort_by_key(|(span, _)| span.lo);
        clashes.extend(located);
    }
    clashes.sort_by_key(|(span, _)| span.lo);
    clashes
}

fn clash_message(name: &str, descriptor: &str, declarations: &[String]) -> String {
    let mut message = format!(
        "platform declaration clash: The following declarations have the same JVM signature ({name}{descriptor}):"
    );
    for declaration in declarations {
        message.push('\n');
        message.push_str("    ");
        message.push_str(declaration);
    }
    message
}

fn value_parameters(function: &crate::ir::IrPackageFunction) -> &[(String, crate::types::Ty)] {
    function
        .params
        .get(function.context_count..)
        .unwrap_or(function.params.as_slice())
}

fn render_declaration(function: &crate::ir::IrPackageFunction, package: Option<&str>) -> String {
    let mut rendered = String::from("fun ");
    if !function.type_params.is_empty() {
        rendered.push('<');
        for (index, parameter) in function.type_params.iter().enumerate() {
            if index > 0 {
                rendered.push_str(", ");
            }
            rendered.push_str(&parameter.name);
            let bounds = parameter
                .bounds
                .iter()
                .copied()
                .filter(|bound| !is_default_bound(*bound))
                .collect::<Vec<_>>();
            if !bounds.is_empty() {
                rendered.push_str(" : ");
                for (bound_index, bound) in bounds.iter().enumerate() {
                    if bound_index > 0 {
                        rendered.push_str(" & ");
                    }
                    rendered.push_str(&bound.source_name());
                }
            }
        }
        rendered.push_str("> ");
    }
    if let Some(receiver) = function.receiver {
        rendered.push_str(&receiver.source_name());
        rendered.push('.');
    }
    rendered.push_str(&function.name);
    rendered.push('(');
    for (index, (name, ty)) in value_parameters(function).iter().enumerate() {
        if index > 0 {
            rendered.push_str(", ");
        }
        rendered.push_str(name);
        rendered.push_str(": ");
        rendered.push_str(&ty.source_name());
    }
    rendered.push_str("): ");
    rendered.push_str(&function.ret.source_name());
    rendered.push_str(" defined in ");
    match package {
        Some(package) if !package.is_empty() => rendered.push_str(package),
        _ => rendered.push_str("root package"),
    }
    rendered
}

/// kotlinc's `MemberComparator` order: name, then rendered receiver and value-parameter types
/// (classifiers qualified), then type-parameter count.
fn declaration_order(
    function: &crate::ir::IrPackageFunction,
) -> (String, String, Vec<String>, usize) {
    (
        function.name.clone(),
        function
            .receiver
            .map(qualified_type_name)
            .unwrap_or_default(),
        value_parameters(function)
            .iter()
            .map(|(_, ty)| qualified_type_name(*ty))
            .collect(),
        function.type_params.len(),
    )
}

fn is_default_bound(bound: crate::types::Ty) -> bool {
    let inner = match bound {
        crate::types::Ty::Nullable(inner) => *inner,
        other => other,
    };
    matches!(
        inner,
        crate::types::Ty::Obj(name, arguments)
            if arguments.is_empty() && name.matches("kotlin/Any")
    )
}

fn qualified_type_name(ty: crate::types::Ty) -> String {
    use crate::types::Ty;
    match ty {
        Ty::Int => "kotlin.Int".to_string(),
        Ty::Byte => "kotlin.Byte".to_string(),
        Ty::Short => "kotlin.Short".to_string(),
        Ty::Long => "kotlin.Long".to_string(),
        Ty::Float => "kotlin.Float".to_string(),
        Ty::Double => "kotlin.Double".to_string(),
        Ty::Boolean => "kotlin.Boolean".to_string(),
        Ty::Char => "kotlin.Char".to_string(),
        Ty::UByte => "kotlin.UByte".to_string(),
        Ty::UShort => "kotlin.UShort".to_string(),
        Ty::UInt => "kotlin.UInt".to_string(),
        Ty::ULong => "kotlin.ULong".to_string(),
        Ty::String => "kotlin.String".to_string(),
        Ty::Unit => "kotlin.Unit".to_string(),
        Ty::Nothing => "kotlin.Nothing".to_string(),
        Ty::Null => "null".to_string(),
        Ty::Obj(name, arguments) => {
            let base = name.render().replace(['/', '$'], ".");
            if arguments.is_empty() {
                base
            } else {
                let arguments = arguments
                    .iter()
                    .map(|argument| qualified_type_name(*argument))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{base}<{arguments}>")
            }
        }
        Ty::Nullable(inner) => {
            let rendered = qualified_type_name(*inner);
            if matches!(*inner, Ty::Fun(_)) {
                format!("({rendered})?")
            } else {
                format!("{rendered}?")
            }
        }
        Ty::PlatformNullable(inner) => format!("{}!", qualified_type_name(*inner)),
        Ty::TyParam(name, _) => crate::types::type_parameter_source_name(name).to_string(),
        Ty::Fun(signature) => {
            let parameters = signature
                .params
                .iter()
                .map(|parameter| qualified_type_name(*parameter))
                .collect::<Vec<_>>()
                .join(", ");
            let suspend = if signature.suspend { "suspend " } else { "" };
            format!(
                "{suspend}({parameters}) -> {}",
                qualified_type_name(signature.ret)
            )
        }
        Ty::InProjection(inner) => format!("in {}", qualified_type_name(*inner)),
        Ty::OutProjection(inner) => format!("out {}", qualified_type_name(*inner)),
        Ty::StarProjection(_) => "*".to_string(),
        Ty::Error | Ty::Pending => String::new(),
    }
}
