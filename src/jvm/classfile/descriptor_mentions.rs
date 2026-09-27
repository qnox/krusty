//! Which classes a class's descriptors and generic signatures name, for `InnerClasses`.
//!
//! kotlinc lists a nested class in `InnerClasses` whenever the class it writes names it in a type
//! position: a class constant, a declared field or method descriptor, a callee's descriptor, or a
//! generic `Signature`. A dependency's nested class seen only in such a position (a parameter of
//! type `Map.Entry`, a callee returning `Thread.State`, a `List<Map.Entry<K, V>>` signature) still
//! gets its row, so the resolver is offered every class those positions name.

use super::ClassWriter;

/// Record every name a `contains("L<name>;")` search over `value` could have matched.
///
/// This is deliberately not a JVM descriptor parser. It preserves the literal predicate used by
/// `InnerClasses` retention, including runs that begin at an `L` inside another name and text that
/// is not itself a valid descriptor.
pub(super) fn record_mentioned_names(
    value: &str,
    names: &mut crate::name_tree::FxHashMap<String, ()>,
) {
    for (index, byte) in value.as_bytes().iter().enumerate() {
        if *byte != b'L' {
            continue;
        }
        let rest = &value[index + 1..];
        if let Some(end) = rest.find(';') {
            names.insert(rest[..end].to_string(), ());
        }
    }
}

/// Record every class a descriptor or generic signature (JVMS §4.7.9.1) names in a type position:
/// each `L…;` class type, each `.Inner` member of a parameterized outer (as `Outer$Inner`), and
/// every class inside type arguments and type-parameter bounds. Type variables (`T…;`) and
/// type-parameter identifiers are not classes. Malformed text stops the walk; what was read up to
/// that point stays recorded.
fn record_signature_classes(value: &str, record: &mut impl FnMut(&str)) {
    let mut reader = SignatureReader {
        bytes: value.as_bytes(),
        value,
        at: 0,
    };
    // `None` marks malformed text: the walk just ends, there is nothing further to read.
    let _ = reader.signature(record);
}

struct SignatureReader<'a> {
    bytes: &'a [u8],
    value: &'a str,
    at: usize,
}

impl SignatureReader<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn eat(&mut self, byte: u8) -> bool {
        let found = self.peek() == Some(byte);
        if found {
            self.at += 1;
        }
        found
    }

    /// A class, method, or field signature, or a plain descriptor.
    fn signature(&mut self, record: &mut impl FnMut(&str)) -> Option<()> {
        if self.peek() == Some(b'<') {
            self.type_parameters(record)?;
        }
        while let Some(byte) = self.peek() {
            match byte {
                b'(' | b')' | b'^' => self.at += 1,
                _ => self.java_type(record)?,
            }
        }
        Some(())
    }

    /// `<Identifier:Bound?(:Bound)*…>`: the identifiers name type parameters, only bounds are types.
    fn type_parameters(&mut self, record: &mut impl FnMut(&str)) -> Option<()> {
        self.at += 1;
        while !self.eat(b'>') {
            let colon = self.value[self.at..].find(':')?;
            self.at += colon;
            while self.eat(b':') {
                if !matches!(self.peek(), Some(b':' | b'>')) {
                    self.java_type(record)?;
                }
            }
        }
        Some(())
    }

    fn java_type(&mut self, record: &mut impl FnMut(&str)) -> Option<()> {
        match self.peek()? {
            b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z' | b'V' | b'*' | b'+' | b'-'
            | b'[' => {
                self.at += 1;
                Some(())
            }
            b'T' => {
                let end = self.value[self.at..].find(';')?;
                self.at += end + 1;
                Some(())
            }
            b'L' => self.class_type(record),
            _ => None,
        }
    }

    /// `L pkg/Outer <args>? (.Inner <args>?)* ;`
    fn class_type(&mut self, record: &mut impl FnMut(&str)) -> Option<()> {
        self.at += 1;
        let mut name = String::new();
        loop {
            let start = self.at;
            while !matches!(self.peek()?, b';' | b'<' | b'.') {
                self.at += 1;
            }
            if !name.is_empty() {
                name.push('$');
            }
            name.push_str(&self.value[start..self.at]);
            record(&name);
            if self.eat(b'<') {
                while !self.eat(b'>') {
                    self.java_type(record)?;
                }
            }
            if self.eat(b';') {
                return Some(());
            }
            if !self.eat(b'.') {
                return None;
            }
        }
    }
}

/// The sizes a [`DescriptorMentionCache`] was computed from: fields, methods, pool entries, and
/// the class signature. Anything appended or set changes one of them.
pub(super) type MentionSizes = (usize, usize, usize, Option<u16>);

pub(super) struct DescriptorMentionCache {
    sizes: MentionSizes,
    /// Every name the retention predicate treats as mentioned.
    names: crate::name_tree::FxHashMap<String, ()>,
    /// The classes the descriptors and signatures name, sorted, for the `InnerClasses` resolver.
    classes: Vec<String>,
}

impl ClassWriter {
    /// Whether a declared member, a typed constant-pool descriptor, or a generic signature
    /// references `internal`.
    ///
    /// For descriptors this is the literal `L<internal>;` search it always was:
    /// [`record_mentioned_names`] records exactly the slices such a search could match, on
    /// well-formed and malformed descriptors alike, without re-scanning per candidate. Generic
    /// signatures nest `<…>` and `.Inner` inside a class type, which no substring search reads, so
    /// they contribute the classes [`record_signature_classes`] parses out of them.
    pub(super) fn descriptor_mentions(&self, internal: &str) -> bool {
        self.mentions(|cache| cache.names.contains_key(internal))
    }

    /// The classes named by this class's descriptors and generic signatures, sorted.
    pub(super) fn signature_classes(&self) -> Vec<String> {
        self.mentions(|cache| cache.classes.clone())
    }

    /// The descriptors of the members and call sites this class's instructions use: the operand of
    /// every field access and `invoke*`, including an `invokedynamic` call site. kotlinc maps
    /// exactly those signatures through its type mapper, which records the nested classes they
    /// name. A bootstrap handle (`LambdaMetafactory.metafactory(MethodHandles.Lookup, …)`) is
    /// written as a raw ASM `Handle` and never mapped, so a member reached only through a handle
    /// contributes nothing, even when an instruction elsewhere shares its `NameAndType`.
    fn referenced_member_descriptors(&self) -> Vec<&str> {
        let mut descriptors = Vec::new();
        for code in self
            .methods
            .iter()
            .filter_map(|method| method.code.as_deref())
        {
            let mut pc = 0;
            while let Some(length) = crate::jvm::bytecode::instruction_len(code, pc) {
                let index = || {
                    code.get(pc + 1..pc + 3)
                        .map(|operand| u16::from_be_bytes([operand[0], operand[1]]))
                };
                let descriptor = match code[pc] {
                    // getstatic, putstatic, getfield, putfield
                    0xb2..=0xb5 => index().and_then(|index| self.cp.fieldref_descriptor(index)),
                    // invokevirtual, invokespecial, invokestatic, invokeinterface
                    0xb6..=0xb9 => index()
                        .and_then(|index| self.cp.methodref_parts(index))
                        .map(|(_, _, descriptor)| descriptor),
                    0xba => index().and_then(|index| self.cp.invokedynamic_descriptor(index)),
                    _ => None,
                };
                descriptors.extend(descriptor);
                pc += length;
            }
        }
        descriptors
    }

    fn mention_sizes(&self) -> MentionSizes {
        (
            self.fields.len(),
            self.methods.len(),
            self.cp.entries.len(),
            self.class_signature,
        )
    }

    /// Run `read` against the memoized mentions, rebuilding them when anything has been appended.
    fn mentions<T>(&self, read: impl Fn(&DescriptorMentionCache) -> T) -> T {
        let sizes = self.mention_sizes();
        if let Some(cached) = self.mentioned_names.borrow().as_ref() {
            if cached.sizes == sizes {
                return read(cached);
            }
        }
        let mut names = crate::name_tree::FxHashMap::default();
        let mut classes = std::collections::BTreeSet::new();
        for index in self
            .fields
            .iter()
            .map(|field| field.desc)
            .chain(self.methods.iter().map(|method| method.desc))
        {
            if let Some(value) = self.cp.utf8_value(index) {
                record_mentioned_names(value, &mut names);
                record_signature_classes(value, &mut |class| {
                    classes.insert(class.to_string());
                });
            }
        }
        self.cp.record_typed_descriptor_names(&mut |value| {
            record_mentioned_names(value, &mut names);
        });
        for value in self.referenced_member_descriptors() {
            record_signature_classes(value, &mut |class| {
                classes.insert(class.to_string());
            });
        }
        for index in self
            .fields
            .iter()
            .filter_map(|field| field.signature)
            .chain(self.methods.iter().filter_map(|method| method.signature))
            .chain(self.class_signature)
        {
            if let Some(value) = self.cp.utf8_value(index) {
                record_signature_classes(value, &mut |class| {
                    classes.insert(class.to_string());
                });
            }
        }
        for class in &classes {
            names.insert(class.clone(), ());
        }
        let cache = DescriptorMentionCache {
            sizes,
            names,
            classes: classes.into_iter().collect(),
        };
        let answer = read(&cache);
        *self.mentioned_names.borrow_mut() = Some(cache);
        answer
    }
}

#[cfg(test)]
mod mentioned_name_tests {
    use super::{record_mentioned_names, record_signature_classes};

    fn mentioned(values: &[&str]) -> Vec<String> {
        let mut names = crate::name_tree::FxHashMap::default();
        for value in values {
            record_mentioned_names(value, &mut names);
        }
        let mut found = names.into_keys().collect::<Vec<_>>();
        found.sort();
        found
    }

    /// The contract is a literal `contains("L<name>;")` search, NOT descriptor parsing. Anything
    /// that search would have matched must still be recorded, or `InnerClasses` retention changes.
    /// A conventional parser is the tempting "cleanup" that these cases exist to block.
    #[test]
    fn a_field_descriptor_records_its_class() {
        assert_eq!(mentioned(&["Ljava/lang/String;"]), ["java/lang/String"]);
    }

    /// An ARRAY descriptor's element class is mentioned exactly as the old search saw it.
    #[test]
    fn an_array_descriptor_records_its_element_class() {
        assert_eq!(mentioned(&["[[Lpkg/Elem;"]), ["pkg/Elem"]);
    }

    /// A method descriptor mentions every class in its parameters AND its result.
    #[test]
    fn a_method_descriptor_records_every_class_it_names() {
        assert_eq!(
            mentioned(&["(Lpkg/A;ILpkg/B;)Lpkg/C;"]),
            ["pkg/A", "pkg/B", "pkg/C"]
        );
    }

    /// Text with no `;` after an `L` matched nothing before and must still match nothing — the
    /// extractor has to tolerate malformed input rather than assume a well-formed descriptor.
    #[test]
    fn unterminated_text_records_nothing() {
        assert_eq!(mentioned(&["Lpkg/Unterminated"]), Vec::<String>::new());
        assert_eq!(mentioned(&["no descriptor here"]), Vec::<String>::new());
    }

    /// The case that rules out a descriptor parser: an internal name CONTAINING `L`.
    ///
    /// `contains("L…;")` could match at either `L` in `Lpkg/LOuter;`, so both runs are recorded —
    /// the properly declared `pkg/LOuter` and the interior `Outer`. A parser would record only the
    /// first, and a class genuinely named `Outer` would then lose its retention.
    #[test]
    fn an_interior_l_records_both_match_points() {
        assert_eq!(mentioned(&["Lpkg/LOuter;"]), ["Outer", "pkg/LOuter"]);
    }

    /// Only the first `;` closes a run, exactly as the substring search bound it.
    #[test]
    fn a_run_stops_at_the_first_semicolon() {
        assert_eq!(mentioned(&["Lpkg/A;Lpkg/B;"]), ["pkg/A", "pkg/B"]);
    }

    /// The constant-pool boundary: only descriptors in a TYPED position are scanned.
    ///
    /// A bare `CONSTANT_Utf8` — a string literal, an attribute name, anything — can spell something
    /// that looks exactly like a descriptor. The old predicate never saw those, because it read
    /// only field/method descriptors and `NameAndType`/`MethodType` entries, and neither does this
    /// one. Widening to every Utf8 in the pool would retain `InnerClasses` rows for classes a
    /// string constant merely mentions.
    #[test]
    fn a_bare_pool_string_is_not_a_mention() {
        let mut writer = super::super::ClassWriter::new("pkg/Owner", "java/lang/Object");
        writer.cp.utf8("Lpkg/OnlyInAStringLiteral;");
        assert!(!writer.descriptor_mentions("pkg/OnlyInAStringLiteral"));

        // The same spelling in a real field descriptor IS a mention, so the exclusion above is the
        // pool position doing the work rather than the name being unreachable.
        writer.add_field(
            super::super::ACC_PUBLIC,
            "field",
            "Lpkg/OnlyInAStringLiteral;",
        );
        assert!(writer.descriptor_mentions("pkg/OnlyInAStringLiteral"));
    }

    fn signature_classes(value: &str) -> Vec<String> {
        let mut found = Vec::new();
        record_signature_classes(value, &mut |class| found.push(class.to_string()));
        found
    }

    /// A type argument's class is a mention even though no `L…;` run spells it on its own.
    #[test]
    fn a_type_argument_names_its_class() {
        assert_eq!(
            signature_classes(
                "(Ljava/util/List<+Ljava/util/Map$Entry<Ljava/lang/String;Ljava/lang/Integer;>;>;)I"
            ),
            [
                "java/util/List",
                "java/util/Map$Entry",
                "java/lang/String",
                "java/lang/Integer"
            ]
        );
    }

    /// An inner class of a parameterized outer is spelled `Outer<…>.Inner`; it names both.
    #[test]
    fn an_inner_member_of_a_parameterized_outer_names_both() {
        assert_eq!(
            signature_classes("Lpkg/Outer<TT;>.Inner<*>;"),
            ["pkg/Outer", "pkg/Outer$Inner"]
        );
    }

    /// Type-parameter identifiers and type variables are not classes, even when spelled `L`.
    #[test]
    fn type_parameters_contribute_only_their_bounds() {
        assert_eq!(
            signature_classes("<L:Ljava/lang/Object;K::Ljava/lang/Comparable<TK;>;>(TL;)TK;"),
            ["java/lang/Object", "java/lang/Comparable"]
        );
    }

    /// A class signature names its superclass and interfaces; a throws clause names its type.
    #[test]
    fn supertypes_and_throws_are_mentions() {
        assert_eq!(
            signature_classes("Ljava/lang/Object;Lpkg/Api<Lpkg/Api$Key;>;"),
            ["java/lang/Object", "pkg/Api", "pkg/Api$Key"]
        );
        assert_eq!(signature_classes("()V^Lpkg/Failure;"), ["pkg/Failure"]);
    }

    /// Malformed text ends the walk without inventing a class from the unread rest.
    #[test]
    fn malformed_text_stops_the_walk() {
        assert_eq!(signature_classes("(Lpkg/A;Q)Lpkg/B;"), ["pkg/A"]);
        assert_eq!(signature_classes("Lpkg/Unterminated"), Vec::<String>::new());
    }

    /// A signature-only mention retains a candidate row: `List<Outer.Nested>` in a field signature
    /// keeps `Outer$Nested` although the field's descriptor names only `java/util/List`.
    #[test]
    fn a_field_signature_is_a_mention() {
        let mut writer = super::super::ClassWriter::new("pkg/Owner", "java/lang/Object");
        writer.add_field_sig(
            super::super::ACC_PUBLIC,
            "field",
            "Ljava/util/List;",
            Some("Ljava/util/List<Lpkg/Outer$Nested;>;"),
        );
        assert!(writer.descriptor_mentions("pkg/Outer$Nested"));
        assert_eq!(
            writer.signature_classes(),
            ["java/util/List", "pkg/Outer$Nested"]
        );
    }

    /// A class whose code calls `make` and which also names `make` as a bootstrap method: both
    /// share one `NameAndType`.
    fn calling_and_bootstrapping(call: bool) -> super::super::ClassWriter {
        let mut writer = super::super::ClassWriter::new("pkg/Owner", "java/lang/Object");
        let make = writer.methodref("pkg/Api", "make", "(Lpkg/Api$Key;)Lpkg/Api$Made;");
        let handle =
            writer.method_handle_static("pkg/Api", "make", "(Lpkg/Api$Key;)Lpkg/Api$Made;");
        writer.add_bootstrap(handle, Vec::new());
        let mut code = super::super::CodeBuilder::new(0);
        if call {
            code.aconst_null();
            code.invokestatic(make, 1, 1);
            code.pop();
        }
        code.ret_void();
        writer.add_method(
            super::super::ACC_PUBLIC | super::super::ACC_STATIC,
            "use",
            "()V",
            &code,
        );
        writer
    }

    /// A member an instruction invokes names the classes in its descriptor, even when a bootstrap
    /// handle names the same member through the same `NameAndType`.
    #[test]
    fn an_invoked_member_is_a_reference_even_when_a_bootstrap_shares_it() {
        assert_eq!(
            calling_and_bootstrapping(true).signature_classes(),
            ["pkg/Api$Key", "pkg/Api$Made"]
        );
    }

    /// kotlinc writes a bootstrap handle raw, so a member only a handle names is not a reference:
    /// the metafactory's `MethodHandles$Lookup` gets no row.
    #[test]
    fn a_member_only_a_bootstrap_names_is_not_a_reference() {
        assert_eq!(
            calling_and_bootstrapping(false).signature_classes(),
            Vec::<String>::new()
        );
    }
}
