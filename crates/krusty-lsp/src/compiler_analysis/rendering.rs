use krusty::ast::TypeRef;
use krusty::types::{Ty, TyKind};

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
    match ty.kind() {
        TyKind::INT => "Int".to_string(),
        TyKind::BYTE => "Byte".to_string(),
        TyKind::SHORT => "Short".to_string(),
        TyKind::LONG => "Long".to_string(),
        TyKind::FLOAT => "Float".to_string(),
        TyKind::DOUBLE => "Double".to_string(),
        TyKind::BOOLEAN => "Boolean".to_string(),
        TyKind::CHAR => "Char".to_string(),
        TyKind::UBYTE => "UByte".to_string(),
        TyKind::USHORT => "UShort".to_string(),
        TyKind::UINT => "UInt".to_string(),
        TyKind::ULONG => "ULong".to_string(),
        TyKind::STRING => "String".to_string(),
        TyKind::Unit => "Unit".to_string(),
        TyKind::Obj(name, arguments) => {
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
        TyKind::Null => "Nothing?".to_string(),
        TyKind::Nothing => "Nothing".to_string(),
        TyKind::Error => "ERROR".to_string(),
        TyKind::Fun(signature) => {
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
        TyKind::Nullable(inner) => render_nullable(inner),
        TyKind::PlatformNullable(inner) => format!("{}!", render_ty(inner)),
        TyKind::DefinitelyNotNull(inner) => format!("{} & Any", render_ty(inner)),
        TyKind::Intersection(parts) => parts
            .iter()
            .copied()
            .map(render_ty)
            .collect::<Vec<_>>()
            .join(" & "),
        TyKind::TyParam(name, _) => name.to_string(),
        TyKind::InProjection(inner) => format!("in {}", render_ty(inner)),
        TyKind::OutProjection(inner) => format!("out {}", render_ty(inner)),
        TyKind::StarProjection(_) => "*".to_string(),
        // A Java raw argument shows as the formal's erasure, the lower bound of the raw interval.
        TyKind::RawProjection(erased) => render_ty(erased),
        // Editor surface: a declaration the resolution engine has not resolved yet has no type to
        // show. It reaches here only while analysis is mid-flight.
        TyKind::Pending => "…".to_string(),
    }
}

/// A nullable intersection is `A? & B?`. A function component is parenthesized so `?` does not
/// bind only to its result.
fn render_nullable(ty: Ty) -> String {
    match ty.kind() {
        TyKind::Intersection(parts) => parts
            .iter()
            .copied()
            .map(|part| {
                let rendered = render_ty(part);
                if part.is_fun() {
                    format!("({rendered})?")
                } else {
                    format!("{rendered}?")
                }
            })
            .collect::<Vec<_>>()
            .join(" & "),
        _ => format!("{}?", render_ty(ty)),
    }
}

#[cfg(test)]
mod tests {
    use super::render_ty;
    use krusty::types::{Ty, TyKind};

    #[test]
    fn a_definitely_non_null_parameter_renders_as_an_intersection() {
        let parameter = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        let intersection = Ty::from(TyKind::DefinitelyNotNull(parameter));
        assert_eq!(render_ty(intersection), "T & Any");
    }

    #[test]
    fn an_intersection_renders_each_component_and_keeps_nullability() {
        let intersection = Ty::intersection(&[Ty::obj("demo/Left"), Ty::obj("demo/Right")]);
        assert_eq!(render_ty(intersection), "Left & Right");
        assert_eq!(render_ty(Ty::nullable(intersection)), "Left? & Right?");
    }
}
