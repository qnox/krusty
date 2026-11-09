//! kotlinc's predefined nullability enhancement of the JDK methods a read-only collection face
//! realizes as stubs (`PREDEFINED_FUNCTION_ENHANCEMENT_INFO_BY_SIGNATURE`).
//!
//! The JDK declares no nullability annotations, so kotlinc enhances these signatures from a table
//! of its own: each position of a method keeps its declared type (`not-platform`), is nullable
//! (`Map.putIfAbsent` returns `V?`), or is definitely non-null (`Map.merge` takes `V & Any`), and
//! a position the table leaves out stays flexible. The enhancement belongs to the declaration, so
//! it applies to the declared signature before a face's type arguments are substituted: `merge` of
//! a `Map<String, S?>` takes an `S` and returns an `S?`. The checked-in table names each method by
//! its exact physical owner, name and descriptor.

use crate::types::{type_name, Ty, TyKind, TypeName};

const PREDEFINED_ENHANCEMENTS_2_4: &str = include_str!("predefined_enhancements_2_4.tsv");

/// kotlinc's `JavaTypeQualifiers` of one type component.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Qualifier {
    /// `NULLABLE`: always `T?`.
    Nullable,
    /// `NOT_PLATFORM`: the declared type, with the nullability its substitution gives it.
    NotPlatform,
    /// `NOT_NULLABLE`: always `T & Any`.
    DefinitelyNotNull,
}

/// The qualifiers of one position, one per component of its declared type in kotlinc's pre-order
/// (the type itself, then each type argument's components); `None` leaves a component flexible.
type PositionQualifiers = Box<[Option<Qualifier>]>;

#[derive(Debug)]
struct PredefinedEnhancement {
    jvm_owner: TypeName,
    physical_name: &'static str,
    descriptor: &'static str,
    result: PositionQualifiers,
    parameters: Box<[PositionQualifiers]>,
}

fn position_qualifiers(field: &str, row: usize) -> PositionQualifiers {
    if field == "-" {
        return Box::new([]);
    }
    field
        .split(',')
        .map(|qualifier| match qualifier {
            "nullable" => Some(Qualifier::Nullable),
            "not-platform" => Some(Qualifier::NotPlatform),
            "not-null" => Some(Qualifier::DefinitelyNotNull),
            "-" => None,
            other => panic!("invalid predefined enhancement qualifier `{other}` in row {row}"),
        })
        .collect()
}

fn predefined_enhancements() -> &'static [PredefinedEnhancement] {
    static ENHANCEMENTS: std::sync::OnceLock<Box<[PredefinedEnhancement]>> =
        std::sync::OnceLock::new();
    ENHANCEMENTS.get_or_init(|| {
        PREDEFINED_ENHANCEMENTS_2_4
            .lines()
            .enumerate()
            .filter_map(|(index, line)| {
                if line.trim().is_empty() || line.starts_with('#') {
                    return None;
                }
                let row = index + 1;
                let mut fields = line.split('\t');
                let (Some(jvm_owner), Some(physical_name), Some(descriptor), Some(result)) =
                    (fields.next(), fields.next(), fields.next(), fields.next())
                else {
                    panic!("invalid predefined enhancement row {row}");
                };
                Some(PredefinedEnhancement {
                    jvm_owner: type_name(jvm_owner),
                    physical_name,
                    descriptor,
                    result: position_qualifiers(result, row),
                    parameters: fields
                        .map(|parameter| position_qualifiers(parameter, row))
                        .collect(),
                })
            })
            .collect::<Vec<_>>()
            .into_boxed_slice()
    })
}

/// The declared parameters and result of the JDK method `physical_name` `descriptor` that
/// `jvm_owner` declares, enhanced as kotlinc's predefined table enhances them. Each enhanced type
/// is still declared: substituting a type argument into it keeps the position's nullability
/// (`V?` stays nullable, `V & Any` drops the argument's `?`, and a flexible `V!` reads the
/// argument as a platform type).
pub fn enhance_jdk_signature(
    jvm_owner: TypeName,
    physical_name: &str,
    descriptor: &str,
    parameters: &[Ty],
    result: Ty,
) -> (Vec<Ty>, Ty) {
    let enhancement = predefined_enhancements().iter().find(|enhancement| {
        enhancement.jvm_owner == jvm_owner
            && enhancement.physical_name == physical_name
            && enhancement.descriptor == descriptor
    });
    fn qualifiers(position: Option<&'static PositionQualifiers>) -> &'static [Option<Qualifier>] {
        position.map_or(&[], |qualifiers| qualifiers)
    }
    let parameters = parameters
        .iter()
        .enumerate()
        .map(|(index, parameter)| {
            let position = enhancement.and_then(|enhancement| enhancement.parameters.get(index));
            enhance(*parameter, qualifiers(position), &mut 0)
        })
        .collect();
    let result = enhance(
        result,
        qualifiers(enhancement.map(|enhancement| &enhancement.result)),
        &mut 0,
    );
    (parameters, result)
}

/// Enhance the type component at `next` and, before it is qualified, each of its type arguments.
fn enhance(ty: Ty, qualifiers: &[Option<Qualifier>], next: &mut usize) -> Ty {
    let qualifier = qualifiers.get(*next).copied().flatten();
    *next += 1;
    let ty = match ty.kind() {
        TyKind::Obj(classifier, arguments) if !arguments.is_empty() => Ty::obj_args_name(
            classifier,
            &arguments
                .iter()
                .map(|argument| enhance_argument(*argument, qualifiers, next))
                .collect::<Vec<_>>(),
        ),
        _ => ty,
    };
    // A primitive or `Unit` position has no nullability to enhance.
    if ty.is_jvm_scalar() || ty == Ty::Unit {
        return ty;
    }
    match qualifier {
        Some(Qualifier::Nullable) => Ty::nullable(ty),
        Some(Qualifier::NotPlatform) => ty,
        Some(Qualifier::DefinitelyNotNull) => match ty.kind() {
            // Substitution reads a definitely non-null occurrence as its argument's non-null form.
            TyKind::TyParam(..) => Ty::from(TyKind::DefinitelyNotNull(ty)),
            _ => ty.definitely_non_null(),
        },
        None => Ty::platform_nullable(ty),
    }
}

/// A type argument occupies one component: a projection's qualifier applies to its bound, and a
/// star projection has no type to enhance.
fn enhance_argument(argument: Ty, qualifiers: &[Option<Qualifier>], next: &mut usize) -> Ty {
    match argument.kind() {
        TyKind::InProjection(bound) => Ty::in_projection(enhance(bound, qualifiers, next)),
        TyKind::OutProjection(bound) => Ty::out_projection(enhance(bound, qualifiers, next)),
        TyKind::StarProjection(_) => {
            *next += 1;
            argument
        }
        _ => enhance(argument, qualifiers, next),
    }
}

#[cfg(test)]
mod tests {
    use super::enhance_jdk_signature;
    use crate::types::{ty_subst_applied_arguments, type_name, Ty};

    fn formal(name: &str) -> Ty {
        Ty::ty_param(name, Ty::obj("kotlin/Any"))
    }

    fn bi_function(arguments: [Ty; 3]) -> Ty {
        Ty::obj_args(
            "java/util/function/BiFunction",
            &[
                Ty::in_projection(arguments[0]),
                Ty::in_projection(arguments[1]),
                Ty::out_projection(arguments[2]),
            ],
        )
    }

    /// `merge` of a `Map<String, S?>`: the value is `S`, the result `S?`, and the remapping
    /// function's arguments and result follow their own qualifiers.
    #[test]
    fn merge_keeps_its_declared_positions_distinct() {
        let (k, v) = (formal("K"), formal("V"));
        let (parameters, result) = enhance_jdk_signature(
            type_name("java/util/Map"),
            "merge",
            "(Ljava/lang/Object;Ljava/lang/Object;Ljava/util/function/BiFunction;)Ljava/lang/Object;",
            &[k, v, bi_function([v, v, v])],
            v,
        );
        let s = Ty::obj("stubs/S");
        let bindings: crate::fx_hash::HashMap<String, Ty> = [
            ("K".to_string(), Ty::obj("kotlin/String")),
            ("V".to_string(), Ty::nullable(s)),
        ]
        .into_iter()
        .collect();
        let applied: Vec<Ty> = parameters
            .iter()
            .map(|parameter| ty_subst_applied_arguments(*parameter, &bindings))
            .collect();
        assert_eq!(
            applied,
            [
                Ty::obj("kotlin/String"),
                s,
                bi_function([s, s, Ty::nullable(s)]),
            ]
        );
        assert_eq!(
            ty_subst_applied_arguments(result, &bindings),
            Ty::nullable(s)
        );
    }

    /// `putIfAbsent` keeps its declared key and value and returns `V?`.
    #[test]
    fn put_if_absent_keeps_its_declared_key() {
        let (k, v) = (formal("K"), formal("V"));
        let (parameters, result) = enhance_jdk_signature(
            type_name("java/util/Map"),
            "putIfAbsent",
            "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
            &[k, v],
            v,
        );
        assert_eq!(parameters, [k, v]);
        assert_eq!(result, Ty::nullable(v));
    }

    /// A method without an entry (`List.sort`), or another owner's method of the same name and
    /// descriptor, stays flexible.
    #[test]
    fn an_unlisted_method_stays_flexible() {
        let e = formal("E");
        let comparator = Ty::obj_args("java/util/Comparator", &[Ty::in_projection(e)]);
        let flexible = Ty::platform_nullable(Ty::obj_args(
            "java/util/Comparator",
            &[Ty::in_projection(Ty::platform_nullable(e))],
        ));
        assert_eq!(
            enhance_jdk_signature(
                type_name("java/util/List"),
                "sort",
                "(Ljava/util/Comparator;)V",
                &[comparator],
                Ty::Unit,
            ),
            (vec![flexible], Ty::Unit)
        );
        assert_eq!(
            enhance_jdk_signature(
                type_name("java/util/LinkedList"),
                "removeFirst",
                "()Ljava/lang/Object;",
                &[],
                e,
            ),
            (vec![], Ty::platform_nullable(e))
        );
    }
}
