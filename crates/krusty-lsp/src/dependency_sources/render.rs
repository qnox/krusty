//! Render classpath declarations as browsable Kotlin source.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use krusty::diag::Span;
use krusty::jvm::classpath::Classpath;
use krusty::jvm::jvm_libraries::JvmLibraries;
use krusty::jvm::metadata::{KotlinMeta, MetaFn, MetaProp};
use krusty::libraries::{LibraryMember, LibraryType, TypeKind};
use krusty::symbol_source::SymbolSource;
use krusty::types::TypeName;

use crate::compiler_analysis::render_ty as render_semantic_ty;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberKey {
    pub name: String,
    pub descriptor: String,
}

pub struct RenderedClass {
    pub text: String,
    pub members: Vec<(MemberKey, Span)>,
    pub type_span: Span,
}

pub enum MaterializedSource {
    Attached { text: String },
    Rendered(RenderedClass),
}

impl MaterializedSource {
    pub fn into_text_and_span(
        self,
        internal: &str,
        member_name: &str,
        member_descriptor: &str,
    ) -> (String, Span) {
        match self {
            MaterializedSource::Rendered(rendered) => {
                let span = rendered
                    .members
                    .iter()
                    .find(|(key, _)| {
                        key.name == member_name
                            && (member_descriptor.is_empty() || key.descriptor == member_descriptor)
                    })
                    .or_else(|| {
                        rendered
                            .members
                            .iter()
                            .find(|(key, _)| key.name == member_name)
                    })
                    .map_or(rendered.type_span, |(_, span)| *span);
                (rendered.text, span)
            }
            MaterializedSource::Attached { text } => {
                let type_name = internal
                    .rsplit('/')
                    .next()
                    .unwrap_or(internal)
                    .split('$')
                    .next()
                    .unwrap_or(internal);
                let span = identifier_span(&text, member_name)
                    .or_else(|| identifier_span(&text, type_name))
                    .unwrap_or_else(|| Span::new(0, 0));
                (text, span)
            }
        }
    }
}

/// Prefer an attached source file and otherwise render a declaration stub.
pub fn materialize(
    classpath: &Rc<Classpath>,
    internal: &str,
    use_sources: bool,
) -> Option<MaterializedSource> {
    if use_sources {
        if let Some(jar) = classpath.owning_jar(internal) {
            if let Some(text) = attached_source(&jar, internal) {
                return Some(MaterializedSource::Attached { text });
            }
        }
    }
    let libraries = JvmLibraries::new(classpath.clone());
    let lib = libraries.resolve_type(internal)?;
    let meta = classpath.find(internal).map(|class| class.meta.clone());
    Some(MaterializedSource::Rendered(render_library_class(
        internal,
        &lib,
        meta.as_ref(),
    )))
}

pub fn render_library_class(
    internal: &str,
    lib: &LibraryType,
    meta: Option<&KotlinMeta>,
) -> RenderedClass {
    let (package, class_name) = match internal.rsplit_once('/') {
        Some((pkg, name)) => (pkg.replace('/', "."), name.to_string()),
        None => (String::new(), internal.to_string()),
    };

    let mut text = String::new();
    if !package.is_empty() {
        text.push_str("package ");
        text.push_str(&package);
        text.push_str("\n\n");
    }

    text.push_str(keyword(lib.kind));
    text.push(' ');

    let type_lo = text.len() as u32;
    text.push_str(&class_name);
    let type_hi = text.len() as u32;

    if !lib.type_params.is_empty() {
        text.push('<');
        text.push_str(&lib.type_params.join(", "));
        text.push('>');
    }

    let supertypes = lib
        .supertypes
        .iter()
        .filter(|name| !name.matches("kotlin/Any") && !name.matches("java/lang/Object"))
        .map(simple_name)
        .collect::<Vec<_>>();
    if !supertypes.is_empty() {
        text.push_str(" : ");
        text.push_str(&supertypes.join(", "));
    }

    text.push_str(" {\n");
    let mut members = Vec::new();
    if lib.kind == TypeKind::Enum && !lib.enum_entries.is_empty() {
        let last = lib.enum_entries.len() - 1;
        for (i, entry) in lib.enum_entries.iter().enumerate() {
            text.push_str("    ");
            let entry_lo = text.len() as u32;
            text.push_str(entry);
            let entry_hi = text.len() as u32;
            members.push((
                MemberKey {
                    name: entry.clone(),
                    descriptor: String::new(),
                },
                Span::new(entry_lo, entry_hi),
            ));
            text.push_str(if i == last { ";\n" } else { ",\n" });
        }
    }
    if let Some(meta) = meta {
        render_properties(&mut text, &mut members, &meta.class_properties);
        render_properties(&mut text, &mut members, &meta.package_properties);
        render_functions(&mut text, &mut members, &meta.class_functions);
        render_functions(&mut text, &mut members, &meta.package_functions);
    } else {
        render_library_members(&mut text, &mut members, &lib.members);
    }
    if lib.companion_object.is_some() {
        text.push_str("    companion object {}\n");
    }
    text.push_str("}\n");

    RenderedClass {
        text,
        members,
        type_span: Span::new(type_lo, type_hi),
    }
}

fn render_library_members(
    text: &mut String,
    members: &mut Vec<(MemberKey, Span)>,
    library_members: &[LibraryMember],
) {
    for member in library_members
        .iter()
        .filter(|member| !member.name.starts_with('<'))
    {
        text.push_str("    fun ");
        let lo = text.len() as u32;
        text.push_str(&member.name);
        let hi = text.len() as u32;
        members.push((
            MemberKey {
                name: member.name.clone(),
                descriptor: member.descriptor.clone(),
            },
            Span::new(lo, hi),
        ));
        text.push('(');
        for (index, parameter) in member.params.iter().enumerate() {
            if index > 0 {
                text.push_str(", ");
            }
            text.push_str(&format!("p{index}: {}", render_semantic_ty(*parameter)));
        }
        text.push_str("): ");
        text.push_str(&render_semantic_ty(member.ret));
        text.push_str(" { TODO() }\n");
    }
}

fn render_functions(text: &mut String, members: &mut Vec<(MemberKey, Span)>, fns: &[MetaFn]) {
    for f in fns {
        text.push_str("    ");
        if f.is_suspend() {
            text.push_str("suspend ");
        }
        if f.is_inline() {
            text.push_str("inline ");
        }
        text.push_str("fun ");
        if let Some(formals) = f.generic_sig.as_ref().map(|sig| &sig.formals) {
            if !formals.is_empty() {
                text.push('<');
                text.push_str(&formals.join(", "));
                text.push_str("> ");
            }
        }
        if let Some(receiver) = f.receiver_class {
            text.push_str(&simple_name(receiver));
            text.push('.');
        }

        let name_lo = text.len() as u32;
        text.push_str(&f.kotlin_name);
        let name_hi = text.len() as u32;
        members.push((
            MemberKey {
                name: f.jvm_name.clone(),
                descriptor: f.jvm_desc.unwrap_or_default().to_string(),
            },
            Span::new(name_lo, name_hi),
        ));

        text.push('(');
        let params: Vec<String> = f
            .value_params
            .iter()
            .map(|p| {
                let vararg = if p.vararg() { "vararg " } else { "" };
                format!("{vararg}{}: {}", p.name, render_type(p.ty))
            })
            .collect();
        text.push_str(&params.join(", "));
        text.push(')');

        if let Some(ret) = f.ret_class {
            text.push_str(": ");
            text.push_str(&render_type(Some(ret)));
            if f.ret_nullable() {
                text.push('?');
            }
        }
        text.push_str(" { TODO() }\n");
    }
}

fn render_properties(text: &mut String, members: &mut Vec<(MemberKey, Span)>, props: &[MetaProp]) {
    for p in props {
        text.push_str("    ");
        if p.is_const {
            text.push_str("const ");
        }
        text.push_str(if p.is_var { "var " } else { "val " });
        if let Some(receiver) = p.receiver_class {
            text.push_str(&simple_name(receiver));
            text.push('.');
        }

        let name_lo = text.len() as u32;
        text.push_str(&p.name);
        let name_hi = text.len() as u32;
        let (key_name, key_desc) = match &p.getter {
            Some(getter) => (getter.name.clone(), getter.desc.clone()),
            None => (p.name.clone(), String::new()),
        };
        members.push((
            MemberKey {
                name: key_name,
                descriptor: key_desc,
            },
            Span::new(name_lo, name_hi),
        ));

        text.push_str(": ");
        text.push_str(&render_type(p.ret_class));
        if p.ret_nullable {
            text.push('?');
        }
        text.push('\n');
    }
}

pub fn attached_source(classes_jar: &Path, internal: &str) -> Option<String> {
    const MAX_ATTACHED_SOURCE_BYTES: u64 = 8 * 1024 * 1024;
    let sources_jar = sources_jar_path(classes_jar)?;
    let file = std::fs::File::open(&sources_jar).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    let (package, class_name) = internal.rsplit_once('/').unwrap_or(("", internal));
    let outer_name = class_name.split('$').next().unwrap_or(class_name);
    let mut names = vec![class_name];
    if outer_name != class_name {
        names.push(outer_name);
    }
    if let Some(facade_name) = outer_name.strip_suffix("Kt") {
        if !names.contains(&facade_name) {
            names.push(facade_name);
        }
    }
    for extension in [".kt", ".java"] {
        for name in &names {
            let separator = if package.is_empty() { "" } else { "/" };
            let entry_name = format!("{package}{separator}{name}{extension}");
            if let Ok(mut entry) = archive.by_name(&entry_name) {
                if entry.size() > MAX_ATTACHED_SOURCE_BYTES {
                    return None;
                }
                let mut text = String::new();
                entry.read_to_string(&mut text).ok()?;
                return Some(text);
            }
        }
    }
    None
}

fn identifier_span(text: &str, identifier: &str) -> Option<Span> {
    if identifier.is_empty() {
        return None;
    }
    text.match_indices(identifier).find_map(|(offset, _)| {
        let before = text[..offset].chars().next_back();
        let after = text[offset + identifier.len()..].chars().next();
        let is_identifier = |ch: char| ch == '_' || ch.is_alphanumeric();
        if before.is_some_and(is_identifier) || after.is_some_and(is_identifier) {
            return None;
        }
        let lo = u32::try_from(offset).ok()?;
        let hi = u32::try_from(offset + identifier.len()).ok()?;
        Some(Span::new(lo, hi))
    })
}

fn sources_jar_path(classes_jar: &Path) -> Option<PathBuf> {
    let stem = classes_jar.file_stem()?.to_str()?;
    Some(classes_jar.with_file_name(format!("{stem}-sources.jar")))
}

fn keyword(kind: TypeKind) -> &'static str {
    match kind {
        TypeKind::Class => "class",
        TypeKind::Interface => "interface",
        TypeKind::Annotation => "annotation class",
        TypeKind::Enum => "enum class",
        TypeKind::Object => "object",
    }
}

fn render_type(ty: Option<TypeName>) -> String {
    match ty {
        Some(tn) => simple_name(tn),
        None => "Any".to_string(),
    }
}

fn simple_name(tn: TypeName) -> String {
    let rendered = tn.render();
    let simple = rendered.rsplit('/').next().unwrap_or(&rendered);
    simple.replace('$', ".")
}
