//! kotlinc's `TypeRemapper` as a `RemappingClassBuilder` applies it: every reference to a class
//! that an inline call regenerates is renamed to the regenerated class, in instructions,
//! descriptors, generic signatures and annotations alike (ASM's `ClassRemapper`).

use std::collections::HashMap;

use crate::jvm::class_node::{Annotation, ElementValue};
use crate::jvm::method_node::{Constant, Handle, Insn, MethodNode, Node};

/// The classes renamed so far, by internal name, and the inline function's type parameters.
#[derive(Clone, Debug, Default)]
pub(crate) struct TypeRemapper {
    mappings: HashMap<String, String>,
    /// Each type parameter's argument at the call site, as a signature; `None` once a signature
    /// declares a type parameter of that name, which shadows it (`AsmTypeRemapper`).
    type_parameters: HashMap<String, Option<String>>,
}

impl TypeRemapper {
    /// A remapper that writes each of the call's type arguments, `(name, signature)`, for its
    /// type parameter (`TypeRemapper.createRoot` over kotlinc's `TypeParameterMappings`).
    pub(crate) fn with_type_arguments(arguments: &[(String, String)]) -> TypeRemapper {
        TypeRemapper {
            mappings: HashMap::new(),
            type_parameters: arguments
                .iter()
                .map(|(name, signature)| (name.clone(), Some(signature.clone())))
                .collect(),
        }
    }

    /// Rename `old` to `new` from now on.
    pub(crate) fn add_mapping(&mut self, old: &str, new: &str) {
        self.mappings.insert(old.to_string(), new.to_string());
    }

    /// `Remapper.map`: an internal name.
    pub(crate) fn map(&self, internal: &str) -> String {
        self.mappings
            .get(internal)
            .cloned()
            .unwrap_or_else(|| internal.to_string())
    }

    /// `Remapper.mapType`: an internal name, or an array class's descriptor.
    pub(crate) fn map_type(&self, class: &str) -> String {
        if class.starts_with('[') {
            self.map_desc(class)
        } else {
            self.map(class)
        }
    }

    /// `Remapper.mapInnerClassName`: the simple name `simple` of the nested class `name` after it
    /// is renamed. A class whose renamed simple name is the same keeps it; otherwise the name after
    /// the renamed class's last `$` and the digits that follow it.
    pub(crate) fn map_inner_class_name(&self, name: &str, simple: &str) -> String {
        let remapped = self.map(name);
        if remapped == name {
            return simple.to_string();
        }
        if let (Some(origin), Some(split)) = (name.rfind('/'), remapped.rfind('/')) {
            if name[origin..] == remapped[split..] {
                return simple.to_string();
            }
        }
        match remapped.rfind('$') {
            Some(dollar) => remapped[dollar + 1..]
                .trim_start_matches(|c: char| c.is_ascii_digit())
                .to_string(),
            None => simple.to_string(),
        }
    }

    /// `Remapper.mapDesc` / `mapMethodDesc`: every class a field or method descriptor names.
    pub(crate) fn map_desc(&self, desc: &str) -> String {
        let mut out = String::with_capacity(desc.len());
        let mut rest = desc;
        while let Some(start) = rest.find('L') {
            out.push_str(&rest[..=start]);
            let tail = &rest[start + 1..];
            let Some(end) = tail.find(';') else {
                out.push_str(tail);
                return out;
            };
            out.push_str(&self.map(&tail[..end]));
            out.push(';');
            rest = &tail[end + 1..];
        }
        out.push_str(rest);
        out
    }

    /// `Remapper.mapSignature`: a generic signature, its class types renamed as
    /// `SignatureRemapper` renames them and each of the call's type parameters replaced by its
    /// argument. The type parameters the signature declares shadow the call's from now on. A
    /// signature that does not parse is returned unchanged.
    pub(crate) fn map_signature(&mut self, signature: &str) -> String {
        let mut remapped = SignatureRemapper {
            remapper: self,
            input: signature.as_bytes(),
            at: 0,
            out: String::with_capacity(signature.len()),
        };
        match remapped.signature() {
            Some(()) if remapped.at == signature.len() => remapped.out,
            _ => signature.to_string(),
        }
    }

    /// `MethodRemapper` over a body: its instructions, handlers and locals.
    pub(crate) fn remap_method(&self, node: &mut MethodNode) {
        node.desc = self.map_desc(&node.desc);
        for entry in &mut node.nodes {
            if let Node::Insn(insn) = entry {
                self.remap_insn(insn);
            }
        }
        for block in &mut node.try_catch_blocks {
            if let Some(class) = &mut block.catch_type {
                *class = self.map_type(class);
            }
        }
        for local in &mut node.local_variables {
            local.desc = self.map_desc(&local.desc);
        }
    }

    pub(crate) fn remap_insn(&self, insn: &mut Insn) {
        match insn {
            Insn::Type { class, .. } => *class = self.map_type(class),
            Insn::Field { owner, desc, .. } => {
                *owner = self.map_type(owner);
                *desc = self.map_desc(desc);
            }
            Insn::Method { owner, desc, .. } => {
                *owner = self.map_type(owner);
                *desc = self.map_desc(desc);
            }
            Insn::InvokeDynamic {
                desc,
                bootstrap,
                arguments,
                ..
            } => {
                *desc = self.map_desc(desc);
                self.remap_handle(bootstrap);
                for argument in arguments {
                    self.remap_constant(argument);
                }
            }
            Insn::Ldc(constant) => self.remap_constant(constant),
            Insn::MultiANewArray { desc, .. } => *desc = self.map_desc(desc),
            Insn::Op(_)
            | Insn::Int { .. }
            | Insn::Var { .. }
            | Insn::Iinc { .. }
            | Insn::Jump { .. }
            | Insn::TableSwitch { .. }
            | Insn::LookupSwitch { .. } => {}
        }
    }

    fn remap_constant(&self, constant: &mut Constant) {
        match constant {
            Constant::Class(class) => *class = self.map_type(class),
            Constant::MethodType(desc) => *desc = self.map_desc(desc),
            Constant::Handle(handle) => self.remap_handle(handle),
            Constant::Int(_)
            | Constant::Float(_)
            | Constant::Long(_)
            | Constant::Double(_)
            | Constant::String(_) => {}
        }
    }

    fn remap_handle(&self, handle: &mut Handle) {
        handle.owner = self.map_type(&handle.owner);
        handle.desc = self.map_desc(&handle.desc);
    }

    /// `AnnotationRemapper`: the annotation's type and every class and enum its values name.
    pub(crate) fn remap_annotation(&self, annotation: &mut Annotation) {
        annotation.desc = self.map_desc(&annotation.desc);
        for (_, value) in &mut annotation.values {
            self.remap_value(value);
        }
    }

    fn remap_value(&self, value: &mut ElementValue) {
        match value {
            ElementValue::Enum(desc, _) => *desc = self.map_desc(desc),
            ElementValue::Class(desc) => *desc = self.map_desc(desc),
            ElementValue::Annotation(annotation) => self.remap_annotation(annotation),
            ElementValue::Array(values) => {
                for value in values {
                    self.remap_value(value);
                }
            }
            ElementValue::Int(..)
            | ElementValue::Long(_)
            | ElementValue::Float(_)
            | ElementValue::Double(_)
            | ElementValue::String(_) => {}
        }
    }
}

/// A walk over a signature (JVMS 4.7.9.1) that copies it, renaming each class type.
struct SignatureRemapper<'a> {
    remapper: &'a mut TypeRemapper,
    input: &'a [u8],
    at: usize,
    out: String,
}

impl SignatureRemapper<'_> {
    fn peek(&self) -> Option<u8> {
        self.input.get(self.at).copied()
    }

    fn copy(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.out.push(byte as char);
        self.at += 1;
        Some(byte)
    }

    /// The identifier up to (not including) any of `stops`.
    fn identifier(&mut self, stops: &[u8]) -> Option<&str> {
        let start = self.at;
        while !stops.contains(&self.peek()?) {
            self.at += 1;
        }
        std::str::from_utf8(&self.input[start..self.at]).ok()
    }

    /// A class, method or field signature.
    fn signature(&mut self) -> Option<()> {
        if self.peek() == Some(b'<') {
            self.type_parameters()?;
        }
        if self.peek() == Some(b'(') {
            self.copy();
            while self.peek()? != b')' {
                self.java_type()?;
            }
            self.copy();
            if self.peek() == Some(b'V') {
                self.copy();
            } else {
                self.java_type()?;
            }
            while self.peek() == Some(b'^') {
                self.copy();
                self.reference_type()?;
            }
            return Some(());
        }
        while self.peek().is_some() {
            self.reference_type()?;
        }
        Some(())
    }

    fn type_parameters(&mut self) -> Option<()> {
        self.copy();
        while self.peek()? != b'>' {
            let name = self.identifier(b":")?.to_string();
            self.out.push_str(&name);
            self.remapper.type_parameters.insert(name, None);
            self.copy(); // the class bound's ':'
                         // A class bound, when there is one, starts a reference type (`SignatureReader`).
            if matches!(self.peek()?, b'L' | b'T' | b'[') {
                self.reference_type()?;
            }
            while self.peek()? == b':' {
                self.copy();
                self.reference_type()?;
            }
        }
        self.copy();
        Some(())
    }

    fn java_type(&mut self) -> Option<()> {
        match self.peek()? {
            b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z' => {
                self.copy();
                Some(())
            }
            _ => self.reference_type(),
        }
    }

    fn reference_type(&mut self) -> Option<()> {
        match self.peek()? {
            b'L' => self.class_type(),
            b'T' => {
                let start = self.at;
                self.at += 1;
                let name = self.identifier(b";")?.to_string();
                self.at += 1;
                match self.remapper.type_parameters.get(&name) {
                    Some(Some(argument)) => self.out.push_str(argument),
                    _ => self
                        .out
                        .push_str(std::str::from_utf8(&self.input[start..self.at]).ok()?),
                }
                Some(())
            }
            b'[' => {
                self.copy();
                self.java_type()
            }
            _ => None,
        }
    }

    /// `L` name type-arguments? (`.` name type-arguments?)* `;`, the outer class renamed as a
    /// whole and each inner class as `SignatureRemapper.visitInnerClassType` renames it.
    fn class_type(&mut self) -> Option<()> {
        self.copy();
        let mut name = self.identifier(b"<.;")?.to_string();
        let mapped = self.remapper.map(&name);
        self.out.push_str(&mapped);
        self.type_arguments()?;
        while self.peek()? == b'.' {
            self.copy();
            let inner = self.identifier(b"<.;")?.to_string();
            let nested = format!("{name}${inner}");
            let outer = std::mem::replace(&mut name, nested);
            let remapped_outer = self.remapper.map(&outer);
            let remapped = self.remapper.map(&name);
            let simple = match remapped.strip_prefix(&remapped_outer) {
                Some(rest) if rest.starts_with('$') => &rest[1..],
                _ => remapped
                    .rsplit_once('$')
                    .map_or(remapped.as_str(), |(_, simple)| simple),
            };
            self.out.push_str(simple);
            self.type_arguments()?;
        }
        (self.copy()? == b';').then_some(())
    }

    fn type_arguments(&mut self) -> Option<()> {
        if self.peek() != Some(b'<') {
            return Some(());
        }
        self.copy();
        while self.peek()? != b'>' {
            match self.peek()? {
                b'*' => {
                    self.copy();
                }
                b'+' | b'-' => {
                    self.copy();
                    self.reference_type()?;
                }
                _ => self.reference_type()?,
            }
        }
        self.copy();
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::TypeRemapper;

    fn remapper() -> TypeRemapper {
        let mut remapper = TypeRemapper::default();
        remapper.add_mapping("lib/A$f$1", "Main$g$$inlined$f$1");
        remapper
    }

    #[test]
    fn descriptors_rename_every_class_they_name() {
        let remapper = remapper();
        assert_eq!(
            remapper.map_desc("(Llib/A$f$1;[Llib/A$f$1;I)Llib/B;"),
            "(LMain$g$$inlined$f$1;[LMain$g$$inlined$f$1;I)Llib/B;"
        );
        assert_eq!(remapper.map_type("[Llib/A$f$1;"), "[LMain$g$$inlined$f$1;");
        assert_eq!(remapper.map_type("lib/A$f$1"), "Main$g$$inlined$f$1");
    }

    #[test]
    fn signatures_rename_class_types_but_not_type_variables() {
        let mut remapper = remapper();
        assert_eq!(
            remapper
                .map_signature("<T:Ljava/lang/Object;>Ljava/lang/Object;Llib/I<Llib/A$f$1;TT;>;"),
            "<T:Ljava/lang/Object;>Ljava/lang/Object;Llib/I<LMain$g$$inlined$f$1;TT;>;"
        );
        assert_eq!(
            remapper.map_signature("(TLlib/A$f$1;)V"),
            "(TLlib/A$f$1;)V",
            "`TLlib/A$f$1;` is the type variable `Llib/A$f$1`, not a class"
        );
        assert_eq!(
            remapper.map_signature("<T::Ljava/lang/Comparable<-TT;>;>(Ljava/util/List<+TT;>;)TT;"),
            "<T::Ljava/lang/Comparable<-TT;>;>(Ljava/util/List<+TT;>;)TT;"
        );
    }

    #[test]
    fn an_inner_class_type_keeps_its_simple_name() {
        let mut remapper = TypeRemapper::default();
        remapper.add_mapping("lib/Outer", "app/Outer");
        remapper.add_mapping("lib/Outer$Inner", "app/Outer$Inner");
        assert_eq!(
            remapper.map_signature("Llib/Outer<TT;>.Inner<TT;>;"),
            "Lapp/Outer<TT;>.Inner<TT;>;"
        );
    }

    #[test]
    fn type_variables_take_the_call_s_arguments_until_a_signature_declares_them() {
        let mut remapper = TypeRemapper::with_type_arguments(&[
            ("T".to_string(), "Ljava/lang/String;".to_string()),
            ("U".to_string(), "TV;".to_string()),
        ]);
        assert_eq!(
            remapper.map_signature("Ljava/lang/Object;Llib/Box<TT;>;"),
            "Ljava/lang/Object;Llib/Box<Ljava/lang/String;>;"
        );
        assert_eq!(
            remapper.map_signature("([TT;)TU;"),
            "([Ljava/lang/String;)TV;"
        );
        assert_eq!(
            remapper.map_signature("<T:Ljava/lang/Object;>(TT;)TU;"),
            "<T:Ljava/lang/Object;>(TT;)TV;"
        );
        assert_eq!(
            remapper.map_signature("()TT;"),
            "()TT;",
            "the declared `T` shadows the call's"
        );
    }
}
