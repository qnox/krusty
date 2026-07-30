use krusty::ast::TypeRef;
use krusty::types::Ty;

pub(crate) fn render_type(reference: &TypeRef) -> String {
    if reference.name == "<fun>" {
        let params = reference
            .fun_params
            .iter()
            .map(render_type)
            .collect::<Vec<_>>()
            .join(", ");
        let result = reference
            .arg
            .as_deref()
            .map_or_else(|| "Unit".to_string(), render_type);
        return format!("({params}) -> {result}");
    }
    let mut result = reference.name.clone();
    if !reference.targs.is_empty() {
        result.push('<');
        result.push_str(
            &reference
                .targs
                .iter()
                .map(render_type)
                .collect::<Vec<_>>()
                .join(", "),
        );
        result.push('>');
    } else if let Some(argument) = &reference.arg {
        result.push('<');
        result.push_str(&render_type(argument));
        result.push('>');
    }
    if reference.nullable() {
        result.push('?');
    }
    result
}

pub(crate) fn render_ty(ty: Ty) -> String {
    match ty {
        Ty::Int => "Int".to_string(),
        Ty::Byte => "Byte".to_string(),
        Ty::Short => "Short".to_string(),
        Ty::Long => "Long".to_string(),
        Ty::Float => "Float".to_string(),
        Ty::Double => "Double".to_string(),
        Ty::Boolean => "Boolean".to_string(),
        Ty::Char => "Char".to_string(),
        Ty::UInt => "UInt".to_string(),
        Ty::ULong => "ULong".to_string(),
        Ty::String => "String".to_string(),
        Ty::Unit => "Unit".to_string(),
        Ty::Obj(name, arguments) => {
            let mut rendered = name
                .render()
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .replace('$', ".");
            if !arguments.is_empty() {
                rendered.push('<');
                rendered.push_str(
                    &arguments
                        .iter()
                        .copied()
                        .map(render_ty)
                        .collect::<Vec<_>>()
                        .join(", "),
                );
                rendered.push('>');
            }
            rendered
        }
        Ty::Null => "Nothing?".to_string(),
        Ty::Nothing => "Nothing".to_string(),
        Ty::Error => "ERROR".to_string(),
        Ty::Fun(signature) => {
            let suspend = if signature.suspend { "suspend " } else { "" };
            let parameters = signature
                .params
                .iter()
                .copied()
                .map(render_ty)
                .collect::<Vec<_>>()
                .join(", ");
            format!("{suspend}({parameters}) -> {}", render_ty(signature.ret))
        }
        Ty::Nullable(inner) => format!("{}?", render_ty(*inner)),
        // Compile-time-constant provenance is invisible to the user — render the underlying type.
        Ty::Const(inner) => render_ty(*inner),
        Ty::TyParam(name, _) => name.to_string(),
    }
}
