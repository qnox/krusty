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
        Ty::UByte => "UByte".to_string(),
        Ty::UShort => "UShort".to_string(),
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
        Ty::Nullable(inner) => render_nullable(*inner),
        Ty::PlatformNullable(inner) => format!("{}!", render_ty(*inner)),
        Ty::DefinitelyNotNull(inner) => format!("{} & Any", render_ty(*inner)),
        Ty::Intersection(parts) => parts
            .iter()
            .copied()
            .map(render_ty)
            .collect::<Vec<_>>()
            .join(" & "),
        Ty::TyParam(name, _) => name.to_string(),
        Ty::InProjection(inner) => format!("in {}", render_ty(*inner)),
        Ty::OutProjection(inner) => format!("out {}", render_ty(*inner)),
        Ty::StarProjection(_) => "*".to_string(),
        // Editor surface: a declaration the resolution engine has not resolved yet has no type to
        // show. It reaches here only while analysis is mid-flight.
        Ty::Pending => "…".to_string(),
    }
}

/// A nullable intersection is `A? & B?`. A function component is parenthesized so `?` does not
/// bind only to its result.
fn render_nullable(ty: Ty) -> String {
    match ty {
        Ty::Intersection(parts) => parts
            .iter()
            .copied()
            .map(|part| {
                let rendered = render_ty(part);
                if matches!(part, Ty::Fun(_)) {
                    format!("({rendered})?")
                } else {
                    format!("{rendered}?")
                }
            })
            .collect::<Vec<_>>()
            .join(" & "),
        other => format!("{}?", render_ty(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::render_ty;
    use krusty::types::{intern_ty, Ty};

    #[test]
    fn a_definitely_non_null_parameter_renders_as_an_intersection() {
        let parameter = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        let intersection = Ty::DefinitelyNotNull(intern_ty(parameter));
        assert_eq!(render_ty(intersection), "T & Any");
    }

    #[test]
    fn an_intersection_renders_each_component_and_keeps_nullability() {
        let intersection = Ty::intersection(&[Ty::obj("demo/Left"), Ty::obj("demo/Right")]);
        assert_eq!(render_ty(intersection), "Left & Right");
        assert_eq!(render_ty(Ty::nullable(intersection)), "Left? & Right?");
    }
}
