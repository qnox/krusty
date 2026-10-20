//! Kotlin class identity fields decoded from the class metadata protobuf.

use super::*;

/// Intern a metadata classifier's serialized identity while preserving its nested declaration
/// boundaries. Kotlin metadata uses `/` through the package and `.` between classifier segments;
/// each nested segment therefore enters the name tree through its semantic owner rather than as a
/// flat spelling that a later phase would have to parse again.
pub(super) fn metadata_classifier_name(internal: &str) -> TypeName {
    let (package, classifiers) = internal.rsplit_once('/').unwrap_or(("", internal));
    let mut classifiers = classifiers.split('.');
    let first = classifiers.next().unwrap_or_default();
    let top = if package.is_empty() {
        crate::types::type_name(first)
    } else {
        crate::types::type_name(&format!("{package}/{first}"))
    };
    classifiers.fold(top, crate::types::type_name_nested_child)
}

pub(super) fn class_kind(flags: u64) -> TypeKind {
    match (flags >> 6) & 0x7 {
        1 => TypeKind::Interface,
        2 => TypeKind::Enum,
        4 => TypeKind::Annotation,
        5 | 6 => TypeKind::Object,
        _ => TypeKind::Class,
    }
}

/// Kotlin `Class.flags`. The protobuf default is PUBLIC FINAL (`6`); decode the word once through
/// this boundary helper so every individual class flag uses identical wire-format defaulting.
pub(super) fn class_flags(ctx: &MetaCtx<'_>) -> u64 {
    let mut flags = 6u64;
    let mut pb = Pb::new(ctx.msg);
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (1, 0) => flags = pb.varint().unwrap_or(6),
            (_, wire) => {
                if pb.skip(wire).is_none() {
                    break;
                }
            }
        }
    }
    flags
}

/// `Class.fq_name` (field 3) as a Kotlin qualified name. Metadata spells a class name with `/`
/// between package segments and `.` between classes (`lib/Outer.Nested`); the qualified name dots
/// both, and neither character can occur inside a JVM identifier.
pub(super) fn class_qualified_name(ctx: &MetaCtx) -> Option<String> {
    let mut pb = Pb::new(ctx.msg);
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (3, 0) => {
                let id = pb.varint()?;
                return resolve_class_name(ctx.records, ctx.d2, id as usize)
                    .map(|name| name.replace('/', "."));
            }
            (_, w) => {
                if pb.skip(w).is_none() {
                    break;
                }
            }
        }
    }
    None
}

/// The simple name of a class's companion object (`Class.companion_object_name = 4`), e.g.
/// `Companion`. `None` if the class has no companion.
pub(super) fn companion_name(ctx: &MetaCtx) -> Option<String> {
    let mut pb = Pb::new(ctx.msg);
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (4, 0) => {
                let id = pb.varint()?;
                return resolve_class_name(ctx.records, ctx.d2, id as usize);
            }
            (_, w) => {
                if pb.skip(w).is_none() {
                    break;
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::metadata_classifier_name;
    use crate::types::Ty;
    use crate::types::TyKind;

    #[test]
    fn nested_metadata_classifier_retains_each_semantic_owner() {
        let deep = metadata_classifier_name("fixture/Outer.Inner.Deep");
        let inner = deep.nested_owner().expect("Deep has its Inner owner");
        let outer = inner.nested_owner().expect("Inner has its Outer owner");

        assert_eq!(deep.nested_segment_ref(), "Deep");
        assert_eq!(inner.nested_segment_ref(), "Inner");
        assert!(outer.matches("fixture/Outer"));
        assert_eq!(outer.nested_owner(), None);

        let TyKind::Obj(decoded, arguments) = super::super::gsig_from_kotlin_class(
            "fixture/Outer.Inner.Deep",
            vec![Ty::Int, Ty::String],
            false,
            0,
        )
        .kind() else {
            panic!("ordinary metadata classifier decodes to an object type");
        };
        assert_eq!(decoded, deep);
        assert_eq!(arguments, &[Ty::Int, Ty::String]);
    }
}
