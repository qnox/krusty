//! The intersection members KSP2 makes where members of two supertypes with one signature meet.
//!
//! KSP2 lists one member for them, made for the classifier: a synthetic copy of the first in
//! supertype order, with its parameters, annotations and place in the source, which overrides that
//! first member. It is abstract when every member it is made from is, and otherwise as final or open
//! as the one that is not, and public. A property's also carries `override`, and its accessors are
//! synthetic and carry no annotations.
//!
//! The round describes one only when KSP2's choice of the member it copies is plain: members of one
//! return type (or property type and mutability), none generic or an extension, none itself made
//! for a supertype, and every one public. Otherwise it leaves the list undescribed.

use krusty_plugin_ir::{
    KspAccessor, KspClassKind, KspDeclaration, KspDeclarationOrigin, KspFunctionKind, KspModifier,
    KspOverridee, KspType, KspTypeArgument, KspTypeSlot,
};

use super::{Entry, Identity, Order, Place};

impl Order<'_> {
    /// The position among `earlier` of the member with `entry`'s signature that `entry` meets,
    /// `Some(None)` when there is none, or `None` when the round cannot say.
    pub(super) fn meeting(&self, earlier: &[Entry], entry: &Entry) -> Option<Option<usize>> {
        let mut meets = None;
        for (position, met) in earlier.iter().enumerate() {
            if met.name == entry.name && self.same_signature(met, entry)? {
                if meets.is_some() {
                    return None;
                }
                meets = Some(position);
            }
        }
        Some(meets)
    }

    /// The member KSP2 makes for the classifier `class` where `parts` meet, or `None` when the
    /// round cannot describe it. `number` tells it apart from the classifier's other ones.
    pub(super) fn intersect(&self, class: &str, parts: Vec<Entry>, number: usize) -> Option<Entry> {
        let mut modalities = Vec::new();
        for part in &parts {
            if matches!(part.identity, Identity::Made(..)) || !self.is_public(part)? {
                return None;
            }
            modalities.push(self.modality(part)?);
        }
        let implemented: Vec<KspModifier> = modalities
            .iter()
            .copied()
            .filter(|modality| *modality != KspModifier::Abstract)
            .collect();
        let modality = match implemented.split_first() {
            None => KspModifier::Abstract,
            Some((first, rest)) if rest.iter().all(|modality| modality == first) => *first,
            Some(_) => return None,
        };
        let first = &parts[0];
        let overridee = KspOverridee::Member(first.member(class));
        let declaration = match self.declaration(first)? {
            KspDeclaration::Function(function) => {
                let return_type = function.return_type.as_ref()?;
                for part in &parts[1..] {
                    let KspDeclaration::Function(other) = self.declaration(part)? else {
                        return None;
                    };
                    if !same_type(return_type, other.return_type.as_ref()?) {
                        return None;
                    }
                }
                if function
                    .parameters
                    .iter()
                    .any(|parameter| !parameter.annotations.is_empty())
                {
                    return None;
                }
                let mut function = function.clone();
                function.origin = KspDeclarationOrigin::Synthetic;
                function.is_abstract = modality == KspModifier::Abstract;
                function.modifiers = Some(vec![modality, KspModifier::Public]);
                function.overridee = overridee;
                KspDeclaration::Function(function)
            }
            KspDeclaration::Property(property) => {
                let ty = property.ty.as_ref()?;
                for part in &parts[1..] {
                    let KspDeclaration::Property(other) = self.declaration(part)? else {
                        return None;
                    };
                    if other.is_mutable != property.is_mutable || !same_type(ty, other.ty.as_ref()?)
                    {
                        return None;
                    }
                }
                // What KSP2 reports for a final one's accessors is not recorded.
                if modality == KspModifier::Final {
                    return None;
                }
                let accessor_modifiers = match modality {
                    KspModifier::Abstract => vec![KspModifier::Abstract, KspModifier::Public],
                    _ => vec![KspModifier::Public],
                };
                let synthesize = |accessor: &KspAccessor| KspAccessor {
                    origin: KspDeclarationOrigin::Synthetic,
                    modifiers: Some(accessor_modifiers.clone()),
                    annotations: Vec::new(),
                    ..accessor.clone()
                };
                let mut property = property.clone();
                property.origin = KspDeclarationOrigin::Synthetic;
                property.modifiers =
                    Some(vec![modality, KspModifier::Override, KspModifier::Public]);
                property.getter = property.getter.as_ref().map(synthesize);
                property.setter = property.setter.as_ref().map(synthesize);
                property.has_backing_field = Some(false);
                property.overridee = overridee;
                KspDeclaration::Property(property)
            }
            _ => return None,
        };
        let file = match &first.place {
            Place::Source { owner, .. } => Some(self.paths.get(owner.as_str())?.to_string()),
            Place::Described { owner, .. } if !self.classes.contains_key(owner.as_str()) => None,
            _ => return None,
        };
        let closure = parts
            .iter()
            .flat_map(|part| std::iter::once(&part.identity).chain(&part.closure))
            .cloned()
            .collect();
        Some(Entry {
            identity: Identity::Made(class.to_string(), format!("intersection {number}")),
            name: first.name.clone(),
            private: false,
            constructor: false,
            closure,
            place: Place::Intersection {
                owner: class.to_string(),
                file,
                declaration: Box::new(declaration),
            },
            parts,
        })
    }

    /// Whether `a` and `b`, of one name, have one signature, or `None` when the round cannot say.
    /// An intersection member has the signature of the members it is made from.
    fn same_signature(&self, a: &Entry, b: &Entry) -> Option<bool> {
        let a = a.parts.first().unwrap_or(a);
        let b = b.parts.first().unwrap_or(b);
        match (self.declaration(a)?, self.declaration(b)?) {
            (KspDeclaration::Function(a), KspDeclaration::Function(b)) => {
                for function in [a, b] {
                    if function.kind != KspFunctionKind::Member
                        || !function.type_parameters.as_ref()?.is_empty()
                        || function.extension_receiver != KspTypeSlot::Absent
                    {
                        return None;
                    }
                }
                if a.parameters.len() != b.parameters.len() {
                    return Some(false);
                }
                for (a, b) in a.parameters.iter().zip(&b.parameters) {
                    if a.is_vararg != b.is_vararg || !same_type(a.ty.as_ref()?, b.ty.as_ref()?) {
                        return Some(false);
                    }
                }
                Some(true)
            }
            (KspDeclaration::Property(a), KspDeclaration::Property(b)) => {
                for property in [a, b] {
                    if !property.type_parameters.as_ref()?.is_empty()
                        || property.extension_receiver != KspTypeSlot::Absent
                    {
                        return None;
                    }
                }
                Some(true)
            }
            _ => None,
        }
    }

    /// The declaration of a listed member, or `None` for an intersection member.
    fn declaration<'e>(&'e self, entry: &'e Entry) -> Option<&'e KspDeclaration> {
        match &entry.place {
            Place::Source { owner, index } => self
                .classes
                .get(owner.as_str())?
                .declarations
                .get(*index as usize),
            Place::Described { declaration, .. } => Some(declaration),
            Place::Intersection { .. } => None,
        }
    }

    /// Whether a listed member is public, or `None` when the round cannot say.
    fn is_public(&self, entry: &Entry) -> Option<bool> {
        let modifiers = self.modifiers(entry)?;
        Some(match entry.place {
            Place::Source { .. } => !modifiers.iter().any(|modifier| {
                matches!(
                    modifier,
                    KspModifier::Private | KspModifier::Protected | KspModifier::Internal
                )
            }),
            _ => modifiers.contains(&KspModifier::Public),
        })
    }

    /// Whether a listed member is abstract, final or open, or `None` when the round cannot say.
    fn modality(&self, entry: &Entry) -> Option<KspModifier> {
        let declaration = self.declaration(entry)?;
        let modifiers = self.modifiers(entry)?;
        let abstract_ = match declaration {
            KspDeclaration::Function(function) => function.is_abstract,
            KspDeclaration::Property(property) => property
                .getter
                .as_ref()
                .and_then(|getter| getter.modifiers.as_ref())
                .is_some_and(|getter| getter.contains(&KspModifier::Abstract)),
            _ => return None,
        };
        if abstract_ || modifiers.contains(&KspModifier::Abstract) {
            return Some(KspModifier::Abstract);
        }
        if modifiers.contains(&KspModifier::Final) {
            return Some(KspModifier::Final);
        }
        if modifiers.contains(&KspModifier::Open) {
            return Some(KspModifier::Open);
        }
        // KSP2 states a source member's modifiers as written: an override or an interface's
        // member is open unless it says otherwise, any other member final.
        let Place::Source { owner, .. } = &entry.place else {
            return None;
        };
        let interface = self.classes.get(owner.as_str())?.kind == KspClassKind::Interface;
        Some(if interface || modifiers.contains(&KspModifier::Override) {
            KspModifier::Open
        } else {
            KspModifier::Final
        })
    }

    fn modifiers<'e>(&'e self, entry: &'e Entry) -> Option<&'e [KspModifier]> {
        match self.declaration(entry)? {
            KspDeclaration::Function(function) => function.modifiers.as_deref(),
            KspDeclaration::Property(property) => property.modifiers.as_deref(),
            _ => None,
        }
    }
}

/// Whether `a` and `b` are one type; whether KSP reports their annotations does not matter.
fn same_type(a: &KspType, b: &KspType) -> bool {
    a.declaration == b.declaration
        && a.nullable == b.nullable
        && a.arguments.len() == b.arguments.len()
        && a.arguments.iter().zip(&b.arguments).all(|pair| match pair {
            (KspTypeArgument::Star, KspTypeArgument::Star) => true,
            (KspTypeArgument::Invariant(a), KspTypeArgument::Invariant(b))
            | (KspTypeArgument::Out(a), KspTypeArgument::Out(b))
            | (KspTypeArgument::In(a), KspTypeArgument::In(b)) => same_type(a, b),
            _ => false,
        })
}
