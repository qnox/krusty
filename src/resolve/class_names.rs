use super::*;

/// Simple type name → JVM internal name, split into a SHARED read-only base (the library/classpath
/// type universe — tens of thousands of stdlib+JDK names, identical for every file on a classpath) and
/// a small per-file `user` overlay (the file's own classes + type aliases). Lookups check `user` first
/// (a user class shadows a classpath type of the same name), then the shared `base`. This avoids
/// cloning the whole base map per compilation — the dominant `collect_signatures` cost before — by
/// sharing it via `Rc`.
#[derive(Clone, Default)]
pub struct ClassNames {
    base: std::rc::Rc<HashMap<String, TypeName>>,
    user: HashMap<String, TypeName>,
    ambiguous: crate::fx_hash::HashSet<String>,
    /// Resolution binding from source lookup spelling to a qualified alias declaration identity.
    pub(super) alias_bindings: HashMap<String, TypeName>,
    /// Expansion templates keyed by qualified alias declaration identity. A source spelling never
    /// acts as semantic identity after its scope/import lookup has selected this key.
    pub(super) alias_expansions: HashMap<TypeName, crate::libraries::AliasExpansion>,
    /// Qualified identity of a SOURCE `typealias` reachable under this spelling.
    ///
    /// Source aliases also have a template during Pass 1. The separate identity edge survives after
    /// the temporary name universe is released so metadata can record `Type.abbreviated_type`.
    pub(super) source_alias_identities: HashMap<String, TypeName>,
    /// First segment that failed while binding a source spelling in this file.
    pub(super) unresolved_segments: HashMap<String, String>,
    /// Zero-based written segment where this file's completed binding first entered a classifier
    /// namespace. Recorded by the name-universe walk; type construction must not reconstruct it.
    classifier_segments: HashMap<String, u32>,
}

pub(super) enum ClassifierBindingError<'a> {
    Ambiguous(&'a str),
    Unresolved(&'a str),
}

impl ClassNames {
    pub fn new(base: std::rc::Rc<HashMap<String, TypeName>>) -> ClassNames {
        ClassNames {
            base,
            user: HashMap::default(),
            ambiguous: crate::fx_hash::HashSet::default(),
            alias_bindings: HashMap::default(),
            alias_expansions: HashMap::default(),
            source_alias_identities: HashMap::default(),
            unresolved_segments: HashMap::default(),
            classifier_segments: HashMap::default(),
        }
    }

    pub fn get(&self, k: &str) -> Option<TypeName> {
        if self.ambiguous.contains(k) {
            return None;
        }
        self.user
            .get(k)
            .copied()
            .or_else(|| self.base.get(k).copied())
    }
    pub fn get_class(&self, k: &str) -> Option<TypeName> {
        self.get(k)
            .filter(|internal| !internal.starts_with("__ty/"))
    }
    pub fn classifier_over_default(&self, name: &str) -> Option<TypeName> {
        classifier_over_default(name, self.get_class(name))
    }
    pub fn contains_key(&self, k: &str) -> bool {
        !self.ambiguous.contains(k) && (self.user.contains_key(k) || self.base.contains_key(k))
    }
    pub(super) fn contains_binding(&self, k: &str) -> bool {
        self.ambiguous.contains(k) || self.user.contains_key(k) || self.base.contains_key(k)
    }
    pub(super) fn classifier_binding<'a>(
        &'a self,
        spelling: &'a str,
    ) -> Result<TypeName, ClassifierBindingError<'a>> {
        if let Some(classifier) = self.get_class(spelling) {
            return Ok(classifier);
        }
        if self.ambiguous.contains(spelling) {
            return Err(ClassifierBindingError::Ambiguous(spelling));
        }
        let mut segments = spelling
            .split(['.', '/'])
            .filter(|segment| !segment.is_empty());
        let Some(root) = segments.next() else {
            return Err(ClassifierBindingError::Unresolved(spelling));
        };
        let scoped_root = if self.ambiguous.contains(root) {
            None
        } else {
            self.user.get(root).copied()
        };
        if let Some(mut owner) = scoped_root.filter(|internal| !internal.starts_with("__ty/")) {
            for segment in segments {
                let Some(child) = crate::types::existing_type_name_nested_child(owner, segment)
                    .filter(|&child| self.has_internal(child))
                else {
                    return Err(ClassifierBindingError::Unresolved(segment));
                };
                owner = child;
            }
            return Ok(owner);
        }
        if self.ambiguous.contains(root) {
            Err(ClassifierBindingError::Ambiguous(root))
        } else {
            Err(ClassifierBindingError::Unresolved(
                self.unresolved_segments
                    .get(spelling)
                    .map_or(root, String::as_str),
            ))
        }
    }
    pub(super) fn unresolved_segment<'a>(&'a self, spelling: &'a str) -> &'a str {
        match self.classifier_binding(spelling) {
            Ok(_) => spelling,
            Err(ClassifierBindingError::Ambiguous(segment))
            | Err(ClassifierBindingError::Unresolved(segment)) => segment,
        }
    }
    fn has_internal(&self, internal: TypeName) -> bool {
        self.user.values().any(|&value| value == internal)
            || self.base.values().any(|&value| value == internal)
    }
    /// Resolve a qualified source supertype, including nested segments below an alias.
    pub fn get_qualified_supertype(&self, name: &str) -> Option<TypeName> {
        if let Some(internal) = self.get(name) {
            return Some(internal);
        }
        if let Some(internal) = self.get(&name.replace('/', ".")) {
            return Some(internal);
        }
        let mut split = name.rfind('/');
        while let Some(position) = split {
            let outer = &name[..position];
            let nested = &name[position + 1..];
            if let Some(base) = self.get(outer) {
                let candidate = nested.split('/').fold(base, type_name_nested_child);
                if self.has_internal(candidate) {
                    return Some(candidate);
                }
            }
            split = outer.rfind('/');
        }
        None
    }
    pub fn library_companion_const(
        &self,
        src: &dyn SymbolSource,
        type_name: &str,
        const_name: &str,
    ) -> Option<crate::libraries::LibraryConst> {
        let owner = src.classifier(self.get(type_name)?)?;
        let companion = owner.companion_object.as_ref()?.1;
        src.classifier(companion)?
            .constants
            .get(const_name)
            .cloned()
    }
    pub(super) fn into_shared(self) -> ClassNames {
        let ClassNames {
            base,
            user,
            ambiguous,
            alias_bindings,
            alias_expansions,
            source_alias_identities,
            unresolved_segments,
            classifier_segments,
        } = self;
        let merged = if base.is_empty() {
            user
        } else {
            let mut merged = (*base).clone();
            merged.extend(user);
            merged
        };
        ClassNames {
            base: std::rc::Rc::new(merged),
            user: HashMap::default(),
            ambiguous,
            alias_bindings,
            alias_expansions,
            source_alias_identities,
            unresolved_segments,
            classifier_segments,
        }
    }

    pub fn insert(&mut self, k: String, v: impl AsRef<str>) -> Option<TypeName> {
        self.insert_name(k, crate::types::type_name(v.as_ref()))
    }
    pub fn insert_name(&mut self, k: String, v: TypeName) -> Option<TypeName> {
        self.ambiguous.remove(&k);
        self.alias_bindings.remove(&k);
        self.source_alias_identities.remove(&k);
        self.user.insert(k, v)
    }
    pub fn mark_ambiguous(&mut self, name: String) {
        self.user.remove(&name);
        self.alias_bindings.remove(&name);
        self.source_alias_identities.remove(&name);
        self.ambiguous.insert(name);
    }

    pub(super) fn insert_unresolved_segment(&mut self, spelling: String, segment: String) {
        self.unresolved_segments.insert(spelling, segment);
    }

    pub(super) fn insert_classifier_segment(&mut self, spelling: String, segment: u32) {
        self.classifier_segments.insert(spelling, segment);
    }

    pub(super) fn classifier_segment(&self, spelling: &str) -> Option<u32> {
        self.classifier_segments.get(spelling).copied()
    }
}

pub(super) fn declared_supertype_name_from_source_name(
    class_source_name: &str,
    name: &str,
    class_names: &ClassNames,
    lexical_classifiers: &[TypeName],
) -> Option<TypeName> {
    // A nested classifier's enclosing classifier rungs precede package/import spellings. In
    // `Counting.Intermediary.Keep : Intermediary`, the nearest declaration is
    // `Counting.Intermediary`, even when another visible `Interpreter.Intermediary` contributes the
    // same simple name. Signature collection must apply the same lexical order as body checking or
    // the stable hierarchy loses the sealed edge before exhaustiveness ever runs.
    // Anonymous and local classifiers have generated `$` segments that are not source nesting.
    // Their real lexical owners come from the parser's structural ownership map. Skip the
    // declaration itself and probe those owners nearest-first before considering file scope.
    for &owner in lexical_classifiers.iter().skip(1) {
        let candidate = type_name_nested_child(owner, name);
        if class_names.has_internal(candidate) {
            return Some(candidate);
        }
    }
    let mut prefix = class_source_name;
    while let Some((enclosing, _)) = prefix.rsplit_once('.') {
        if let Some(internal) = class_names.get(&format!("{enclosing}.{name}")) {
            return Some(internal);
        }
        prefix = enclosing;
    }
    class_names.get_qualified_supertype(name)
}

pub(super) fn declared_supertype_name(
    class: &ClassDecl,
    name: &str,
    class_names: &ClassNames,
    lexical_classifiers: &[TypeName],
) -> Option<TypeName> {
    declared_supertype_name_from_source_name(&class.name, name, class_names, lexical_classifiers)
}

/// Supertypes implied by a Kotlin classifier kind rather than written in its source header.
/// They are semantic Kotlin identities; a backend maps them to its physical platform types.
pub(super) fn implicit_source_supertypes(class: &ClassDecl) -> impl Iterator<Item = TypeName> {
    class
        .is_annotation()
        .then(|| type_name("kotlin/Annotation"))
        .into_iter()
}
