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
    resolve_class_name(ctx.records, ctx.d2, class_name_index(ctx)?)
        .map(|name| name.replace('/', "."))
}

pub(super) fn class_declaration_name(
    ctx: &MetaCtx,
    string_table: &[u8],
) -> MetadataResult<Option<crate::types::ClassifierDeclarationName>> {
    let Some(id) = class_name_index(ctx) else {
        return Ok(None);
    };
    let local = super::string_table::class_name_is_local(string_table, id)
        .ok_or(MetadataDecodeError::MalformedWire)?;
    if local || flags_visibility(class_flags(ctx)) == 5 {
        return Ok(None);
    }
    Ok(resolve_class_name(ctx.records, ctx.d2, id)
        .map(|name| crate::libraries::metadata_declaration_name(&name)))
}

fn class_name_index(ctx: &MetaCtx) -> Option<usize> {
    let mut pb = Pb::new(ctx.msg);
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (3, 0) => {
                return usize::try_from(pb.varint()?).ok();
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

/// The direct subclasses of a `sealed` class, from its `@Metadata` — `Class.sealedSubclassFqName` (field
/// 16, a repeated `QualifiedName` index). Returned as JVM internal names (`lib/D$A`), so an exhaustive
/// `when` over a CLASSPATH sealed subject can be proven exhaustive the same way a same-module one is. Only
/// a sealed class records these, so a non-empty result also implies `is_sealed`.
pub(super) fn sealed_subclasses(ctx: &MetaCtx) -> Vec<String> {
    let records = ctx.records;
    let d2 = ctx.d2;
    let mut out = Vec::new();
    let push_id = |id: usize, out: &mut Vec<String>| {
        if let Some(name) = resolve_class_name(records, d2, id) {
            // A metadata class name spells a nested type with `.` (`lib/D.A`) and the package with `/`;
            // the JVM internal name uses `$` for nesting (`lib/D$A`). Convert only after the last `/`.
            let internal = match name.rfind('/') {
                Some(slash) => {
                    format!("{}{}", &name[..=slash], name[slash + 1..].replace('.', "$"))
                }
                None => name.replace('.', "$"),
            };
            if !out.contains(&internal) {
                out.push(internal);
            }
        }
    };
    let mut pb = Pb::new(ctx.msg);
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            // sealedSubclassFqName = 16, unpacked repeated varint.
            (16, 0) => {
                if let Some(id) = pb.varint() {
                    push_id(id as usize, &mut out);
                }
            }
            // packed repeated form.
            (16, 2) => {
                if let Some(n) = pb.varint() {
                    if let Some(bytes) = pb.bytes(n as usize) {
                        let mut ip = Pb::new(bytes);
                        while let Some(id) = ip.varint() {
                            push_id(id as usize, &mut out);
                        }
                    }
                }
            }
            (_, w) => {
                if pb.skip(w).is_none() {
                    break;
                }
            }
        }
    }
    out
}

/// The simple names of the classifiers a class declares in its body, in declaration order:
/// `Class.nested_class_name` (field 7, a repeated string-table index, packed or not). The
/// `InnerClasses` attribute lists them in an order of its own.
pub(super) fn nested_class_names(ctx: &MetaCtx) -> Box<[Box<str>]> {
    let mut out = Vec::new();
    let mut push = |id: u64| {
        if let Some(name) = resolve_string(ctx.records, ctx.d2, id as usize) {
            out.push(name.into_boxed_str());
        }
    };
    let mut pb = Pb::new(ctx.msg);
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (7, 0) => {
                if let Some(id) = pb.varint() {
                    push(id);
                }
            }
            (7, 2) => {
                let Some(bytes) = pb.varint().and_then(|n| pb.bytes(n as usize)) else {
                    break;
                };
                let mut ids = Pb::new(bytes);
                while let Some(id) = ids.varint() {
                    push(id);
                }
            }
            (_, w) => {
                if pb.skip(w).is_none() {
                    break;
                }
            }
        }
    }
    out.into_boxed_slice()
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
