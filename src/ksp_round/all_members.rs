//! What `KSClassDeclaration.getAllFunctions` and `getAllProperties` list for a source classifier,
//! in KSP2's order.
//!
//! KSP2 lists the classifier's own functions (or properties) in declaration order, then the members
//! of each direct supertype in written order as that supertype lists them, leaving out a private
//! one, one already listed, and one a member listed before overrides. A data class's generated
//! `equals`, `hashCode` and `toString` take the place of the member each overrides. Constructors
//! come last. A member inherited along two supertypes that are not one declaration becomes an
//! intersection override of KSP2's own, and a member of a supertype applied to type arguments a
//! substituted copy; neither is described yet, so neither is a classifier with a library supertype
//! other than `kotlin.Any`, an enum class, or an annotation class: the round leaves their lists
//! undescribed and the host refuses the queries.

use std::cell::RefCell;

use krusty_plugin_ir::{
    KspClass, KspClassKind, KspClasspath, KspDeclaration, KspDeclarationOrigin, KspFile,
    KspFunction, KspFunctionKind, KspMember, KspModifier, KspTypeDeclaration,
};

use super::KspLibraries;
use crate::frontend::{FrontendDeclarationId, OverriddenMember, SourceSetAnalysis};
use crate::fx_hash::{HashMap, HashSet};
use crate::types::{wk, TypeName};

const ANY: &str = "kotlin.Any";

/// What the round learned about each source classifier while describing it, by qualified name.
#[derive(Default)]
pub(super) struct Ledger(RefCell<HashMap<String, ClassLedger>>);

/// What one source classifier's member lists are built from.
pub(super) struct ClassLedger {
    /// The resolved declaration of each of the classifier's declarations that is a member function
    /// or property the source writes.
    pub(super) resolved: Vec<Option<FrontendDeclarationId>>,
    /// The members the compiler generates that KSP2 lists among the inherited ones.
    pub(super) generated: Vec<KspFunction>,
}

impl Ledger {
    pub(super) fn record(&self, qualified_name: String, class: ClassLedger) {
        self.0.borrow_mut().insert(qualified_name, class);
    }
}

/// Fills in the member lists of every source classifier of `files` that the round can order.
pub(super) fn order(files: &mut [KspFile], ledger: Ledger, analysis: &SourceSetAnalysis) {
    let ledger = ledger.0.into_inner();
    let lists = {
        let mut classes = HashMap::default();
        for file in files.iter() {
            collect(file.classes(), &mut classes);
        }
        let mut order = Order {
            classes,
            ledger: &ledger,
            analysis,
            any: any_functions(analysis),
            closures: HashMap::default(),
            lists: HashMap::default(),
        };
        let names: Vec<&str> = order.classes.keys().copied().collect();
        names
            .into_iter()
            .map(|name| {
                let functions = order.list(name, Kind::Functions);
                let properties = order.list(name, Kind::Properties);
                let members = |entries: Option<std::rc::Rc<Vec<Entry>>>| {
                    entries.map(|entries| entries.iter().map(|entry| entry.member(name)).collect())
                };
                (name.to_string(), (members(properties), members(functions)))
            })
            .collect::<HashMap<String, MemberLists>>()
    };
    for file in files {
        assign(&mut file.declarations, &lists);
    }
}

/// A classifier's properties and functions as it lists them, or `None` for a list the round leaves
/// unordered.
type MemberLists = (Option<Vec<KspMember>>, Option<Vec<KspMember>>);

fn collect<'a>(
    classes: impl Iterator<Item = &'a KspClass>,
    into: &mut HashMap<&'a str, &'a KspClass>,
) {
    for class in classes {
        into.insert(&class.qualified_name, class);
        collect(class.classes(), into);
    }
}

fn assign(declarations: &mut [KspDeclaration], lists: &HashMap<String, MemberLists>) {
    for declaration in declarations {
        if let KspDeclaration::Class(class) = declaration {
            if let Some((properties, functions)) = lists.get(&class.qualified_name) {
                class.all_properties = properties.clone();
                class.all_functions = functions.clone();
            }
            assign(&mut class.declarations, lists);
        }
    }
}

/// `kotlin.Any`'s functions as KSP2 lists them, constructors left out.
fn any_functions(analysis: &SourceSetAnalysis) -> Option<Vec<Entry>> {
    let any = KspLibraries::new(analysis).class(ANY)?;
    let mut entries = Vec::new();
    for member in any.class.all_functions.as_ref()? {
        let KspMember::Declared(index) = member else {
            return None;
        };
        let KspDeclaration::Function(function) = &any.class.declarations[*index as usize] else {
            return None;
        };
        if function.kind != KspFunctionKind::Member {
            continue;
        }
        entries.push(Entry {
            identity: Identity::Library(wk::any(), function.name.as_str().into()),
            name: function.name.clone(),
            private: false,
            constructor: false,
            closure: Vec::new(),
            place: Place::Described {
                owner: ANY.to_string(),
                declaration: Box::new(KspDeclaration::Function(function.clone())),
            },
        });
    }
    Some(entries)
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    Functions,
    Properties,
}

/// Which member an entry is, for telling one member listed twice and one member overriding
/// another.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Identity {
    /// A member function or property the source writes.
    Declared(FrontendDeclarationId),
    /// A library classifier's member, by owner and name.
    Library(TypeName, Box<str>),
    /// A member the compiler made, by its classifier and its place among the classifier's
    /// declarations or, for one KSP2 lists only among the members, by name.
    Made(String, String),
}

/// Where the host finds a listed member.
#[derive(Clone)]
enum Place {
    /// The declaration at `index` of the source classifier `owner`.
    Source { owner: String, index: u32 },
    /// A declaration described with the list, which `owner` declares.
    Described {
        owner: String,
        declaration: Box<KspDeclaration>,
    },
}

#[derive(Clone)]
struct Entry {
    identity: Identity,
    name: String,
    private: bool,
    /// Whether it is a constructor, which a classifier does not inherit.
    constructor: bool,
    /// Every member this one overrides, directly or not.
    closure: Vec<Identity>,
    place: Place,
}

impl Entry {
    /// The member as the classifier `class` lists it.
    fn member(&self, class: &str) -> KspMember {
        match &self.place {
            Place::Source { owner, index } if owner == class => KspMember::Declared(*index),
            Place::Source { owner, index } => KspMember::Source {
                owner: owner.clone(),
                index: *index,
            },
            Place::Described { owner, declaration } => KspMember::Inherited {
                owner: owner.clone(),
                declaration: declaration.clone(),
            },
        }
    }

    /// Whether the generated member `name` overrides this one: `kotlin.Any`'s member of that name
    /// or a member overriding it.
    fn is_any_slot(&self, name: &str) -> bool {
        let any = Identity::Library(wk::any(), name.into());
        self.name == name && (self.identity == any || self.closure.contains(&any))
    }
}

struct Order<'a> {
    classes: HashMap<&'a str, &'a KspClass>,
    ledger: &'a HashMap<String, ClassLedger>,
    analysis: &'a SourceSetAnalysis,
    any: Option<Vec<Entry>>,
    closures: HashMap<FrontendDeclarationId, Vec<Identity>>,
    /// Each list worked out, `None` while it is being worked out or when it cannot be.
    lists: HashMap<(String, Kind), Option<std::rc::Rc<Vec<Entry>>>>,
}

impl Order<'_> {
    /// The members of `kind` the classifier `name` lists, or `None` when the round cannot order
    /// them.
    fn list(&mut self, name: &str, kind: Kind) -> Option<std::rc::Rc<Vec<Entry>>> {
        let key = (name.to_string(), kind);
        if let Some(list) = self.lists.get(&key) {
            return list.clone();
        }
        self.lists.insert(key.clone(), None);
        let list = self.work_out(name, kind).map(std::rc::Rc::new);
        self.lists.insert(key, list.clone());
        list
    }

    fn work_out(&mut self, name: &str, kind: Kind) -> Option<Vec<Entry>> {
        let class = *self.classes.get(name)?;
        let ledger = self.ledger.get(name)?;
        if !matches!(
            class.kind,
            KspClassKind::Class | KspClassKind::Interface | KspClassKind::Object
        ) || !class.declarations_complete
        {
            return None;
        }
        let mut own = Vec::new();
        let mut constructors = Vec::new();
        for (index, declaration) in class.declarations.iter().enumerate() {
            let place = Place::Source {
                owner: name.to_string(),
                index: index as u32,
            };
            let (member_name, origin, modifiers) = match (declaration, kind) {
                (KspDeclaration::Function(function), Kind::Functions)
                    if function.kind == KspFunctionKind::Member =>
                {
                    (&function.name, function.origin, &function.modifiers)
                }
                (KspDeclaration::Function(function), Kind::Functions)
                    if matches!(
                        function.kind,
                        KspFunctionKind::PrimaryConstructor | KspFunctionKind::SecondaryConstructor
                    ) =>
                {
                    constructors.push(place);
                    continue;
                }
                (KspDeclaration::Property(property), Kind::Properties) => {
                    (&property.name, property.origin, &property.modifiers)
                }
                _ => continue,
            };
            let identity = match (ledger.resolved[index], origin) {
                (Some(declaration), _) => Identity::Declared(declaration),
                (None, KspDeclarationOrigin::Synthetic) => {
                    Identity::Made(name.to_string(), index.to_string())
                }
                // A member the source writes that did not resolve cannot say what it overrides.
                (None, _) => return None,
            };
            let closure = self.closure(&identity);
            own.push(Entry {
                identity,
                name: member_name.clone(),
                private: modifiers.as_ref()?.contains(&KspModifier::Private),
                constructor: false,
                closure,
                place,
            });
        }
        let mut generated: Vec<&KspFunction> = match kind {
            Kind::Functions => ledger.generated.iter().collect(),
            Kind::Properties => Vec::new(),
        };
        let mut overridden: HashSet<Identity> = own
            .iter()
            .flat_map(|entry| entry.closure.iter().cloned())
            .collect();
        let mut listed: HashSet<Identity> =
            own.iter().map(|entry| entry.identity.clone()).collect();
        let mut inherited: Vec<Entry> = Vec::new();
        for super_type in class.super_types.as_ref()? {
            let KspTypeDeclaration::Class(super_name) = &super_type.declaration else {
                return None;
            };
            if !super_type.arguments.is_empty() {
                return None;
            }
            let entries = if super_name == ANY {
                match kind {
                    Kind::Functions => std::rc::Rc::new(self.any.clone()?),
                    Kind::Properties => std::rc::Rc::new(Vec::new()),
                }
            } else {
                self.list(super_name, kind)?
            };
            for entry in entries.iter() {
                if entry.constructor || entry.private || overridden.contains(&entry.identity) {
                    continue;
                }
                if let Some(position) = generated
                    .iter()
                    .position(|function| entry.is_any_slot(&function.name))
                {
                    let function = generated.remove(position);
                    let mut closure = vec![entry.identity.clone()];
                    closure.extend(entry.closure.iter().cloned());
                    overridden.extend(closure.iter().cloned());
                    inherited.push(Entry {
                        identity: Identity::Made(name.to_string(), function.name.clone()),
                        name: function.name.clone(),
                        private: false,
                        constructor: false,
                        closure,
                        place: Place::Described {
                            owner: name.to_string(),
                            declaration: Box::new(KspDeclaration::Function(function.clone())),
                        },
                    });
                    continue;
                }
                if listed.contains(&entry.identity) {
                    continue;
                }
                let intersects = entry
                    .closure
                    .iter()
                    .any(|identity| listed.contains(identity))
                    || inherited
                        .iter()
                        .any(|listed| listed.closure.contains(&entry.identity));
                if intersects {
                    return None;
                }
                listed.insert(entry.identity.clone());
                inherited.push(entry.clone());
            }
        }
        if !generated.is_empty() {
            return None;
        }
        let mut entries = own;
        entries.extend(inherited);
        entries.extend(constructors.into_iter().map(|place| Entry {
            identity: Identity::Made(name.to_string(), String::new()),
            name: String::new(),
            private: false,
            constructor: true,
            closure: Vec::new(),
            place,
        }));
        Some(entries)
    }

    /// Every member `identity` overrides, directly or not, as the frontend selected them.
    fn closure(&mut self, identity: &Identity) -> Vec<Identity> {
        let Identity::Declared(declaration) = identity else {
            return Vec::new();
        };
        if let Some(closure) = self.closures.get(declaration) {
            return closure.clone();
        }
        self.closures.insert(*declaration, Vec::new());
        let mut closure = Vec::new();
        if let Some(owner) = self.analysis.member_owner(*declaration) {
            for overridden in self.analysis.overridden_members(owner, *declaration) {
                let overridden = match overridden {
                    OverriddenMember::Declared(declared) => Identity::Declared(declared),
                    OverriddenMember::Library { owner, name } => Identity::Library(owner, name),
                };
                let deeper = self.closure(&overridden);
                closure.push(overridden);
                closure.extend(deeper);
            }
        }
        self.closures.insert(*declaration, closure.clone());
        closure
    }
}
