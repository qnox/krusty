//! Member types a Java source class can name by simple name: those it declares and those it
//! inherits from its supertypes (JLS §8.5).
//!
//! Membership is structural. A member type is found by its declaring classifier and simple name,
//! never by composing `Owner$Name` and testing whether that spelling exists: `$` is legal inside a
//! Java identifier, so a top-level `Base$Req` is not a member of `Base`. Inheritance applies Java
//! accessibility: a private member type is never inherited, and a package-private one only into a
//! class of the same package.

use super::super::classreader::{visibility_of_access, ClassInfo};
use crate::fx_hash::{HashMap, HashSet};
use crate::types::{TypeName, Visibility};
use std::cell::RefCell;

/// The classifiers outside the Java sources being stubbed, named by JVM internal name.
pub trait JavaHeaderWorld {
    /// Whether this world declares the classifier.
    fn contains(&self, internal: &str) -> bool;
    /// The member type `simple_name` that `owner` itself declares.
    fn member_type(&self, owner: &str, simple_name: &str) -> Option<JavaMemberType>;
    /// Direct supertypes of a classifier [`Self::contains`] accepted.
    fn direct_supertypes(&self, internal: &str) -> Vec<String>;
}

/// A member type as its declaring classifier publishes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JavaMemberType {
    pub internal: String,
    pub visibility: Visibility,
}

/// A world of top-level classifiers known by name only: none declares a member type or a
/// supertype.
impl<F: Fn(&str) -> bool> JavaHeaderWorld for F {
    fn contains(&self, internal: &str) -> bool {
        self(internal)
    }

    fn member_type(&self, _owner: &str, _simple_name: &str) -> Option<JavaMemberType> {
        None
    }

    fn direct_supertypes(&self, _internal: &str) -> Vec<String> {
        Vec::new()
    }
}

impl JavaHeaderWorld for super::super::classpath::Classpath {
    fn contains(&self, internal: &str) -> bool {
        self.find(internal).is_some()
    }

    fn member_type(&self, owner: &str, simple_name: &str) -> Option<JavaMemberType> {
        classfile_member_type(&*self.find(owner)?, owner, simple_name)
    }

    fn direct_supertypes(&self, internal: &str) -> Vec<String> {
        self.find(internal)
            .map_or_else(Vec::new, |class| classfile_supertypes(&class))
    }
}

/// Classfiles a build already emitted, in front of the classpath it compiled against.
pub struct EmittedClassesHeaderWorld<'a> {
    emitted: HashMap<String, ClassInfo>,
    classpath: &'a super::super::classpath::Classpath,
}

impl<'a> EmittedClassesHeaderWorld<'a> {
    /// `None` when an emitted classfile does not parse.
    pub fn new<'b>(
        emitted: impl IntoIterator<Item = &'b [u8]>,
        classpath: &'a super::super::classpath::Classpath,
    ) -> Option<Self> {
        let emitted = emitted
            .into_iter()
            .map(|bytes| {
                let class = super::super::classreader::parse_class(bytes).ok()?;
                Some((class.this_class(), class))
            })
            .collect::<Option<_>>()?;
        Some(Self { emitted, classpath })
    }
}

impl JavaHeaderWorld for EmittedClassesHeaderWorld<'_> {
    fn contains(&self, internal: &str) -> bool {
        self.emitted.contains_key(internal) || self.classpath.contains(internal)
    }

    fn member_type(&self, owner: &str, simple_name: &str) -> Option<JavaMemberType> {
        match self.emitted.get(owner) {
            Some(class) => classfile_member_type(class, owner, simple_name),
            None => self.classpath.member_type(owner, simple_name),
        }
    }

    fn direct_supertypes(&self, internal: &str) -> Vec<String> {
        match self.emitted.get(internal) {
            Some(class) => classfile_supertypes(class),
            None => self.classpath.direct_supertypes(internal),
        }
    }
}

/// The owner's `InnerClasses` entry naming `simple_name` as one of its members.
fn classfile_member_type(
    class: &ClassInfo,
    owner: &str,
    simple_name: &str,
) -> Option<JavaMemberType> {
    class
        .inner_classes
        .iter()
        .find(|entry| {
            entry.outer.as_deref() == Some(owner) && entry.name.as_deref() == Some(simple_name)
        })
        .map(|entry| JavaMemberType {
            internal: entry.inner.clone(),
            visibility: visibility_of_access(entry.access),
        })
}

fn classfile_supertypes(class: &ClassInfo) -> Vec<String> {
    class
        .super_class()
        .into_iter()
        .chain(class.interfaces())
        .collect()
}

/// A classpath together with the classifiers of the module's Kotlin sources. Their nesting and
/// visibility come from the Pass-1 inventory; their supertypes resolve only after the Java headers
/// are built, so the member types they declare are visible and the ones they inherit are not.
pub struct SourceModuleHeaderWorld<'a> {
    pub source_classifiers: &'a HashMap<TypeName, Visibility>,
    pub classpath: &'a super::super::classpath::Classpath,
}

impl SourceModuleHeaderWorld<'_> {
    fn source_classifier(&self, internal: &str) -> Option<TypeName> {
        crate::types::existing_type_name(internal)
            .filter(|name| self.source_classifiers.contains_key(name))
    }
}

impl JavaHeaderWorld for SourceModuleHeaderWorld<'_> {
    fn contains(&self, internal: &str) -> bool {
        self.source_classifier(internal).is_some() || self.classpath.contains(internal)
    }

    fn member_type(&self, owner: &str, simple_name: &str) -> Option<JavaMemberType> {
        let Some(owner) = self.source_classifier(owner) else {
            return self.classpath.member_type(owner, simple_name);
        };
        let member = owner.existing_nested_child(simple_name)?;
        let visibility = *self.source_classifiers.get(&member)?;
        Some(JavaMemberType {
            // The classfile name javac will see for the nested classifier.
            internal: member.render(),
            visibility,
        })
    }

    fn direct_supertypes(&self, internal: &str) -> Vec<String> {
        if self.source_classifier(internal).is_some() {
            return Vec::new();
        }
        self.classpath.direct_supertypes(internal)
    }
}

/// The outcome of looking a simple name up among one class's member types.
pub(super) enum MemberTypeLookup {
    Found(String),
    Absent,
    Ambiguous,
}

/// Declaration provenance remains available while access is checked at each inheritance edge.
struct InheritedMemberType {
    declaring_owner: String,
    member: JavaMemberType,
}

/// Member-type lookup over the parsed Java sources and the world beyond them.
pub(super) struct MemberTypes<'a> {
    world: &'a dyn JavaHeaderWorld,
    /// `(declaring classifier, simple name)` → the first normalized source member header.
    declared: HashMap<(&'a str, &'a str), JavaMemberType>,
    /// Direct supertypes already resolved for a classifier; `None` while they are being resolved,
    /// so a cyclic `extends` reads as declaring none instead of recursing.
    supertypes: RefCell<HashMap<String, Option<Vec<String>>>>,
}

impl<'a> MemberTypes<'a> {
    pub(super) fn new(
        world: &'a dyn JavaHeaderWorld,
        members: impl IntoIterator<Item = ((&'a str, &'a str), JavaMemberType)>,
    ) -> Self {
        let mut declared = HashMap::default();
        for (key, member) in members {
            declared.entry(key).or_insert(member);
        }
        Self {
            world,
            declared,
            supertypes: RefCell::default(),
        }
    }

    /// The member type `name` in a lexical class scope, declared or inherited. A directly
    /// declared private type is visible from that class's own source scope.
    /// `source_supertypes` resolves a parsed source declaration's direct supertypes and answers
    /// `None` for any other classifier.
    pub(super) fn lookup(
        &self,
        class: &str,
        name: &str,
        source_supertypes: &dyn Fn(&str) -> Option<Vec<String>>,
    ) -> MemberTypeLookup {
        selected_member(self.candidates(class, name, source_supertypes))
    }

    /// A member selected through a type qualifier. Membership belongs to the qualifier's class;
    /// access belongs to the referencing declaration, supplied by its lexical resolver. An
    /// inaccessible alternative does not remove an ambiguity or reveal a hidden declaration.
    pub(super) fn lookup_qualified(
        &self,
        class: &str,
        name: &str,
        source_supertypes: &dyn Fn(&str) -> Option<Vec<String>>,
        accessible: &dyn Fn(&str, Visibility) -> bool,
    ) -> MemberTypeLookup {
        let found = self.candidates(class, name, source_supertypes);
        if let [candidate] = found.as_slice() {
            if !accessible(&candidate.declaring_owner, candidate.member.visibility) {
                return MemberTypeLookup::Absent;
            }
        }
        selected_member(found)
    }

    fn candidates(
        &self,
        class: &str,
        name: &str,
        source_supertypes: &dyn Fn(&str) -> Option<Vec<String>>,
    ) -> Vec<InheritedMemberType> {
        if let Some(member) = self.declared_member_type(class, name) {
            return vec![InheritedMemberType {
                declaring_owner: class.to_string(),
                member,
            }];
        }
        let mut visited = HashSet::default();
        visited.insert(class.to_string());
        let mut found = Vec::new();
        for supertype in self.direct_supertypes(class, source_supertypes) {
            self.collect_inherited(
                &supertype,
                name,
                package_of(class),
                source_supertypes,
                &mut visited,
                &mut found,
            );
        }
        found
    }

    /// Whether the parsed or external supertype graph proves this exact classifier is a
    /// subclass of `base`. Lexically enclosing subclass scopes may access protected member types.
    pub(super) fn is_subclass_of(
        &self,
        class: &str,
        base: &str,
        source_supertypes: &dyn Fn(&str) -> Option<Vec<String>>,
    ) -> bool {
        let mut pending = vec![class.to_string()];
        let mut visited = HashSet::default();
        while let Some(current) = pending.pop() {
            if current == base {
                return true;
            }
            if visited.insert(current.clone()) {
                pending.extend(self.direct_supertypes(&current, source_supertypes));
            }
        }
        false
    }

    /// Member types `class` makes available to a subclass in `package`: its own accessible ones,
    /// else those it inherits in turn. Accessibility is decided at each inheritance edge, so the
    /// member types `class` inherits are those accessible from `class`'s own package: a
    /// package-private member type an intermediate class in another package cannot inherit is not
    /// resurrected when the chain returns to the declaring package.
    fn collect_inherited(
        &self,
        class: &str,
        name: &str,
        package: &str,
        source_supertypes: &dyn Fn(&str) -> Option<Vec<String>>,
        visited: &mut HashSet<String>,
        found: &mut Vec<InheritedMemberType>,
    ) {
        if !visited.insert(class.to_string()) {
            return;
        }
        let mut inherited = Vec::new();
        if let Some(member) = self.declared_member_type(class, name) {
            inherited.push(InheritedMemberType {
                declaring_owner: class.to_string(),
                member,
            });
        } else {
            let inheriting_package = package_of(class);
            for supertype in self.direct_supertypes(class, source_supertypes) {
                self.collect_inherited(
                    &supertype,
                    name,
                    inheriting_package,
                    source_supertypes,
                    visited,
                    &mut inherited,
                );
            }
        }
        for candidate in inherited {
            if inheritable(&candidate.member, &candidate.declaring_owner, package)
                && !found
                    .iter()
                    .any(|known| known.member.internal == candidate.member.internal)
            {
                found.push(candidate);
            }
        }
        visited.remove(class);
    }

    fn declared_member_type(&self, owner: &str, name: &str) -> Option<JavaMemberType> {
        if let Some(member) = self.declared.get(&(owner, name)) {
            return Some(member.clone());
        }
        self.world.member_type(owner, name)
    }

    fn direct_supertypes(
        &self,
        class: &str,
        source_supertypes: &dyn Fn(&str) -> Option<Vec<String>>,
    ) -> Vec<String> {
        if let Some(known) = self.supertypes.borrow().get(class) {
            return known.clone().unwrap_or_default();
        }
        self.supertypes.borrow_mut().insert(class.to_string(), None);
        let resolved =
            source_supertypes(class).unwrap_or_else(|| self.world.direct_supertypes(class));
        self.supertypes
            .borrow_mut()
            .insert(class.to_string(), Some(resolved.clone()));
        resolved
    }
}

fn selected_member(mut found: Vec<InheritedMemberType>) -> MemberTypeLookup {
    match found.len() {
        0 => MemberTypeLookup::Absent,
        1 => MemberTypeLookup::Found(found.pop().unwrap().member.internal),
        _ => MemberTypeLookup::Ambiguous,
    }
}

/// JLS §8.5: a class inherits the non-private member types of its supertypes that are accessible
/// to code in it; a package-private one is accessible only within its declaring package.
fn inheritable(member: &JavaMemberType, declaring: &str, package: &str) -> bool {
    match member.visibility {
        Visibility::Private => false,
        Visibility::PackagePrivate => package_of(declaring) == package,
        Visibility::Public | Visibility::Internal | Visibility::Protected => true,
    }
}

/// The package of a JVM internal name. `/` separates package segments only, so this is exact even
/// when a class name contains `$`.
fn package_of(internal: &str) -> &str {
    internal.rsplit_once('/').map_or("", |(package, _)| package)
}
