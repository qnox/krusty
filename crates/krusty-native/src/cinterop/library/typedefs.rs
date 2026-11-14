//! A C typedef as Kotlin/Native binds it: a type alias for the Kotlin value type and, for a
//! typedef of a scalar, pointer or enum, a second alias `<name>Var` for the variable holding one.
//! A typedef of a struct or function type has only the first.

use super::super::model::TypedefId;
use super::kotlin_type::KotlinType;
use super::mirror::{pointed_of, Mapper, Mirror};
use super::stub::TypeAlias;

/// The aliases of a registered typedef; `None` when its type has no Kotlin representation.
pub(super) fn typedef_aliases(mapper: &Mapper<'_>, id: TypedefId) -> Option<Vec<TypeAlias>> {
    let typedef = mapper.declarations.typedef(id);
    let name = &typedef.name;
    Some(match mapper.mirror(typedef.ty).ok()? {
        Mirror::ByValue { value, info, .. } => {
            let alias = KotlinType::alias(mapper.own(name), Vec::new(), value.clone());
            vec![
                TypeAlias {
                    name: format!("{name}Var"),
                    underlying: pointed_of(&info, alias),
                },
                TypeAlias {
                    name: name.to_string(),
                    underlying: value,
                },
            ]
        }
        Mirror::ByRef { pointed } => vec![TypeAlias {
            name: name.to_string(),
            underlying: pointed,
        }],
    })
}
