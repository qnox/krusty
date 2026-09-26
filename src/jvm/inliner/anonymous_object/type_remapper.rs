//! kotlinc's `TypeRemapper` as a `RemappingClassBuilder` applies it: every reference to a class
//! that an inline call regenerates is renamed to the regenerated class, in instructions,
//! descriptors, generic signatures and annotations alike (ASM's `ClassRemapper`).

use std::collections::HashMap;

use crate::jvm::class_node::{Annotation, ElementValue};
use crate::jvm::method_node::{Constant, Handle, Insn, MethodNode, Node};

/// A descriptor, internal name or generic signature that does not parse. The regenerated class
/// would keep whatever part of it names the original, so the whole regeneration is declined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MalformedType(pub String);

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
    pub(crate) fn map_type(&self, class: &str) -> Result<String, MalformedType> {
        if class.starts_with('[') {
            self.map_desc(class)
        } else if class.is_empty() || class.contains(';') {
            Err(MalformedType(class.to_string()))
        } else {
            Ok(self.map(class))
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

    /// `Remapper.mapDesc` / `mapMethodDesc`: every class a field or method descriptor names
    /// (`V` stands for `void.class` in an annotation).
    pub(crate) fn map_desc(&self, desc: &str) -> Result<String, MalformedType> {
        let malformed = || MalformedType(desc.to_string());
        let bytes = desc.as_bytes();
        let mut out = String::with_capacity(desc.len());
        let end = if desc == "V" {
            out.push('V');
            1
        } else if bytes.first() == Some(&b'(') {
            out.push('(');
            let mut at = 1;
            while bytes.get(at) != Some(&b')') {
                at = self
                    .map_field_type(desc, at, &mut out)
                    .ok_or_else(malformed)?;
            }
            out.push(')');
            at += 1;
            if bytes.get(at) == Some(&b'V') {
                out.push('V');
                at + 1
            } else {
                self.map_field_type(desc, at, &mut out)
                    .ok_or_else(malformed)?
            }
        } else {
            self.map_field_type(desc, 0, &mut out)
                .ok_or_else(malformed)?
        };
        if end != desc.len() {
            return Err(malformed());
        }
        Ok(out)
    }

    /// The field type of `desc` at `at`, renamed onto `out`; where it ends.
    fn map_field_type(&self, desc: &str, mut at: usize, out: &mut String) -> Option<usize> {
        let bytes = desc.as_bytes();
        while bytes.get(at) == Some(&b'[') {
            out.push('[');
            at += 1;
        }
        match *bytes.get(at)? {
            byte @ (b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z') => {
                out.push(byte as char);
                Some(at + 1)
            }
            b'L' => {
                let end = at + 1 + desc[at + 1..].find(';')?;
                let name = &desc[at + 1..end];
                if name.is_empty() {
                    return None;
                }
                out.push('L');
                out.push_str(&self.map(name));
                out.push(';');
                Some(end + 1)
            }
            _ => None,
        }
    }

    /// `Remapper.mapSignature`: a generic signature, its class types renamed as
    /// `SignatureRemapper` renames them and each of the call's type parameters replaced by its
    /// argument. The type parameters a signature declares shadow the call's for the rest of the
    /// class, not only in that signature: kotlinc registers them in the regenerated class's own
    /// `TypeRemapper` (`AsmTypeRemapper.visitFormalTypeParameter`), so a member after
    /// `fun <T> f()` keeps `TT;` where the call's `T` was meant (checked against kotlinc 2.4.20).
    /// A signature that does not parse registers nothing.
    pub(crate) fn map_signature(&mut self, signature: &str) -> Result<String, MalformedType> {
        let registered = self.type_parameters.clone();
        let mut remapped = SignatureRemapper {
            remapper: self,
            input: signature.as_bytes(),
            at: 0,
            out: String::with_capacity(signature.len()),
        };
        match remapped.signature() {
            Some(()) if remapped.at == signature.len() => Ok(remapped.out),
            _ => {
                self.type_parameters = registered;
                Err(MalformedType(signature.to_string()))
            }
        }
    }

    /// `MethodRemapper` over a body: its instructions, handlers and locals.
    pub(crate) fn remap_method(&self, node: &mut MethodNode) -> Result<(), MalformedType> {
        node.desc = self.map_desc(&node.desc)?;
        for entry in &mut node.nodes {
            if let Node::Insn(insn) = entry {
                self.remap_insn(insn)?;
            }
        }
        for block in &mut node.try_catch_blocks {
            if let Some(class) = &mut block.catch_type {
                *class = self.map_type(class)?;
            }
        }
        for local in &mut node.local_variables {
            local.desc = self.map_desc(&local.desc)?;
        }
        Ok(())
    }

    pub(crate) fn remap_insn(&self, insn: &mut Insn) -> Result<(), MalformedType> {
        match insn {
            Insn::Type { class, .. } => *class = self.map_type(class)?,
            Insn::Field { owner, desc, .. } | Insn::Method { owner, desc, .. } => {
                *owner = self.map_type(owner)?;
                *desc = self.map_desc(desc)?;
            }
            Insn::InvokeDynamic {
                desc,
                bootstrap,
                arguments,
                ..
            } => {
                *desc = self.map_desc(desc)?;
                self.remap_handle(bootstrap)?;
                for argument in arguments {
                    self.remap_constant(argument)?;
                }
            }
            Insn::Ldc(constant) => self.remap_constant(constant)?,
            Insn::MultiANewArray { desc, .. } => *desc = self.map_desc(desc)?,
            Insn::Op(_)
            | Insn::Int { .. }
            | Insn::Var { .. }
            | Insn::Iinc { .. }
            | Insn::Jump { .. }
            | Insn::TableSwitch { .. }
            | Insn::LookupSwitch { .. } => {}
        }
        Ok(())
    }

    fn remap_constant(&self, constant: &mut Constant) -> Result<(), MalformedType> {
        match constant {
            Constant::Class(class) => *class = self.map_type(class)?,
            Constant::MethodType(desc) => *desc = self.map_desc(desc)?,
            Constant::Handle(handle) => self.remap_handle(handle)?,
            Constant::Int(_)
            | Constant::Float(_)
            | Constant::Long(_)
            | Constant::Double(_)
            | Constant::String(_) => {}
        }
        Ok(())
    }

    fn remap_handle(&self, handle: &mut Handle) -> Result<(), MalformedType> {
        handle.owner = self.map_type(&handle.owner)?;
        handle.desc = self.map_desc(&handle.desc)?;
        Ok(())
    }

    /// `AnnotationRemapper`: the annotation's type and every class and enum its values name.
    pub(crate) fn remap_annotation(
        &self,
        annotation: &mut Annotation,
    ) -> Result<(), MalformedType> {
        annotation.desc = self.map_desc(&annotation.desc)?;
        for (_, value) in &mut annotation.values {
            self.remap_value(value)?;
        }
        Ok(())
    }

    fn remap_value(&self, value: &mut ElementValue) -> Result<(), MalformedType> {
        match value {
            ElementValue::Enum(desc, _) | ElementValue::Class(desc) => {
                *desc = self.map_desc(desc)?
            }
            ElementValue::Annotation(annotation) => self.remap_annotation(annotation)?,
            ElementValue::Array(values) => {
                for value in values {
                    self.remap_value(value)?;
                }
            }
            ElementValue::Int(..)
            | ElementValue::Long(_)
            | ElementValue::Float(_)
            | ElementValue::Double(_)
            | ElementValue::String(_) => {}
        }
        Ok(())
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
    use super::{MalformedType, TypeRemapper};

    fn ok(text: &str) -> Result<String, MalformedType> {
        Ok(text.to_string())
    }

    fn malformed(text: &str) -> Result<String, MalformedType> {
        Err(MalformedType(text.to_string()))
    }

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
            ok("(LMain$g$$inlined$f$1;[LMain$g$$inlined$f$1;I)Llib/B;")
        );
        assert_eq!(
            remapper.map_type("[Llib/A$f$1;"),
            ok("[LMain$g$$inlined$f$1;")
        );
        assert_eq!(remapper.map_type("lib/A$f$1"), ok("Main$g$$inlined$f$1"));
    }

    #[test]
    fn signatures_rename_class_types_but_not_type_variables() {
        let mut remapper = remapper();
        assert_eq!(
            remapper
                .map_signature("<T:Ljava/lang/Object;>Ljava/lang/Object;Llib/I<Llib/A$f$1;TT;>;"),
            ok("<T:Ljava/lang/Object;>Ljava/lang/Object;Llib/I<LMain$g$$inlined$f$1;TT;>;")
        );
        assert_eq!(
            remapper.map_signature("(TLlib/A$f$1;)V"),
            ok("(TLlib/A$f$1;)V"),
            "`TLlib/A$f$1;` is the type variable `Llib/A$f$1`, not a class"
        );
        assert_eq!(
            remapper.map_signature("<T::Ljava/lang/Comparable<-TT;>;>(Ljava/util/List<+TT;>;)TT;"),
            ok("<T::Ljava/lang/Comparable<-TT;>;>(Ljava/util/List<+TT;>;)TT;")
        );
    }

    #[test]
    fn an_inner_class_type_keeps_its_simple_name() {
        let mut remapper = TypeRemapper::default();
        remapper.add_mapping("lib/Outer", "app/Outer");
        remapper.add_mapping("lib/Outer$Inner", "app/Outer$Inner");
        assert_eq!(
            remapper.map_signature("Llib/Outer<TT;>.Inner<TT;>;"),
            ok("Lapp/Outer<TT;>.Inner<TT;>;")
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
            ok("Ljava/lang/Object;Llib/Box<Ljava/lang/String;>;")
        );
        assert_eq!(
            remapper.map_signature("([TT;)TU;"),
            ok("([Ljava/lang/String;)TV;")
        );
        assert_eq!(
            remapper.map_signature("<T:Ljava/lang/Object;>(TT;)TU;"),
            ok("<T:Ljava/lang/Object;>(TT;)TV;")
        );
        assert_eq!(
            remapper.map_signature("()TT;"),
            ok("()TT;"),
            "the declared `T` shadows the call's for the rest of the class, as in kotlinc"
        );
    }

    #[test]
    fn a_malformed_descriptor_is_refused_whole() {
        let remapper = remapper();
        for desc in [
            "Llib/A$f$1",
            "(Llib/A$f$1;",
            "(Llib/A$f$1;)",
            "()Llib/A$f$1;I",
            "L;",
            "[",
            "Q",
            "(V)V",
            "",
        ] {
            assert_eq!(remapper.map_desc(desc), malformed(desc), "{desc:?}");
        }
        assert_eq!(
            remapper.map_desc("V"),
            ok("V"),
            "`void.class` in an annotation"
        );
    }

    #[test]
    fn a_malformed_internal_name_is_refused() {
        let remapper = remapper();
        for class in ["", "lib/A$f$1;", "[Llib/A$f$1"] {
            assert_eq!(remapper.map_type(class), malformed(class), "{class:?}");
        }
    }

    #[test]
    fn a_malformed_signature_declares_no_type_parameter() {
        let mut remapper = TypeRemapper::with_type_arguments(&[(
            "T".to_string(),
            "Ljava/lang/String;".to_string(),
        )]);
        assert_eq!(
            remapper.map_signature("<T:Ljava/lang/Object;>(TT;"),
            malformed("<T:Ljava/lang/Object;>(TT;")
        );
        assert_eq!(remapper.map_signature("()TT;"), ok("()Ljava/lang/String;"));
    }

    #[test]
    fn a_malformed_signature_is_refused_whole() {
        let mut remapper = TypeRemapper::with_type_arguments(&[(
            "T".to_string(),
            "Ljava/lang/String;".to_string(),
        )]);
        remapper.add_mapping("lib/A$f$1", "Main$g$$inlined$f$1");
        for signature in [
            "Ljava/lang/Object;Llib/I<Llib/A$f$1;TT;>",
            "Ljava/lang/Object;Llib/I<Llib/A$f$1;",
            "(TT;",
            "(TT)V",
            "<T>Ljava/lang/Object;",
            "()TT;extra",
        ] {
            assert_eq!(
                remapper.map_signature(signature),
                malformed(signature),
                "{signature:?}"
            );
        }
    }
}
