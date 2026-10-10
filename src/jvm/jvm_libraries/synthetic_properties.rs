//! Kotlin synthetic properties of Java accessor methods, derived from the method declarations.
//!
//! A Java method declares a synthetic property through its own spelling: `getX` reads as the
//! decapitalized `x`, and `isX` reads as `isX` itself. The provider inventories the methods a
//! classifier (or a hierarchy) declares once and indexes them by the property each one declares;
//! a property lookup then reads that index instead of guessing method spellings from the requested
//! property name. Arity, return type and static ownership are checked on the declaration that the
//! index names. The setter of a property is named from its getter's own spelling.

use std::collections::{HashMap, HashSet};

use crate::symbol_source::SymbolSource;
use crate::types::Ty;

/// The synthetic property the Java accessor `method` declares, from its spelling alone: `getUrl` →
/// `url`, `getURLPath` → `urlPath`, `getIsOpen` → `isOpen`, `isOpen` → `isOpen`. A lowercase letter
/// after the prefix (`getaway`, `island`) declares nothing.
pub(super) fn accessor_property_name(method: &str) -> Option<String> {
    if let Some(stem) = method.strip_prefix("is") {
        if stem.chars().next().is_some_and(char::is_uppercase) {
            return Some(method.to_string());
        }
    }
    get_prefixed_property_name(method)
}

/// Kotlin's decapitalize-smart mapping of a `get`-prefixed accessor: a LEADING UPPERCASE RUN
/// lowercases as a block (`getID` → `id`, `getURLPath` → `urlPath`).
fn get_prefixed_property_name(method: &str) -> Option<String> {
    let stem = method.strip_prefix("get")?;
    if !stem.is_ascii() {
        let mut chars = stem.chars();
        let first = chars.next()?;
        if !first.is_uppercase() {
            return None;
        }
        return Some(format!("{}{}", first.to_lowercase(), chars.as_str()));
    }
    let first = stem.as_bytes().first()?;
    if !first.is_ascii_uppercase() {
        return None;
    }
    let bytes = stem.as_bytes();
    let lower = bytes.iter().position(u8::is_ascii_lowercase);
    let prefix = match lower {
        None => bytes.len(),
        Some(0) => return None,
        Some(1) => 1,
        Some(index) => index - 1,
    };
    Some(format!(
        "{}{}",
        stem[..prefix].to_ascii_lowercase(),
        &stem[prefix..]
    ))
}

/// The setter paired with the getter `getter`: `set` plus the getter's spelling without its
/// accessor prefix. `isOpen` pairs with `setOpen`, `getIsOpen` with `setIsOpen`, and `getURLPath`
/// with `setURLPath`.
pub(super) fn setter_name_for_getter(getter: &str) -> String {
    let stem = if accessor_property_name(getter).as_deref() == Some(getter) {
        &getter["is".len()..]
    } else {
        getter.strip_prefix("get").unwrap_or(getter)
    };
    format!("set{stem}")
}

/// The accessor methods of one declaration scope, indexed by the synthetic property each declares.
#[derive(Debug, Default)]
pub(super) struct AccessorInventory {
    getters: HashMap<String, Vec<String>>,
}

impl AccessorInventory {
    /// Inventory the given method spellings. Each property keeps its getters in kotlinc's lookup
    /// order: the `isX` method itself, then the `get` spellings in declaration order.
    pub(super) fn from_methods<'a>(methods: impl IntoIterator<Item = &'a str>) -> Self {
        let mut getters: HashMap<String, Vec<String>> = HashMap::new();
        for method in methods {
            let Some(property) = accessor_property_name(method) else {
                continue;
            };
            let spellings = getters.entry(property).or_default();
            if !spellings.iter().any(|spelling| spelling == method) {
                spellings.push(method.to_string());
            }
        }
        for (property, spellings) in &mut getters {
            spellings.sort_by_key(|spelling| accessor_precedence(property, spelling));
        }
        Self { getters }
    }

    /// The getter methods that declare `property`.
    pub(super) fn getters(&self, property: &str) -> &[String] {
        self.getters.get(property).map_or(&[], Vec::as_slice)
    }
}

/// Kotlin's candidate order among real accessor declarations for one property: an `isX` spelling,
/// then the conventional `getX` capitalization, then alternative leading-uppercase-run spellings
/// such as `getURLPath`. This classifies declarations already present in the inventory; it never
/// manufactures a method name to retry lookup.
fn accessor_precedence(property: &str, getter: &str) -> u8 {
    if getter == property {
        return 0;
    }
    let Some(stem) = getter.strip_prefix("get") else {
        return 2;
    };
    let mut chars = stem.chars();
    let Some(first) = chars.next() else {
        return 2;
    };
    let conventional = first.to_lowercase().chain(chars).eq(property.chars());
    if conventional {
        1
    } else {
        2
    }
}

/// Inventory every method declared through `receiver`'s hierarchy. A Java classifier's synthetic
/// property pairs a getter and a setter wherever in its supertypes each one is declared, so the
/// index spans the whole hierarchy rather than one declaring classifier.
pub(super) fn hierarchy_inventory(source: &dyn SymbolSource, receiver: Ty) -> AccessorInventory {
    let mut methods = Vec::new();
    let mut seen = HashSet::new();
    let mut pending = vec![receiver.non_null()];
    while let Some(current) = pending.pop() {
        let Some(internal) = current.kotlin_class_internal() else {
            continue;
        };
        if !seen.insert(internal) {
            continue;
        }
        let Some(classifier) = source.classifier(internal) else {
            continue;
        };
        // This inventory is entered only for a non-Kotlin receiver. Methods inherited by that Java
        // classifier are part of its Java surface even when Kotlin metadata owns the declaration
        // in a superclass (`JavaSubclass : KotlinBase`); the direct Kotlin receiver is rejected at
        // the provider entry point before this walk begins.
        methods.extend(
            classifier
                .members
                .iter()
                .filter(|member| !member.is_member_extension())
                .map(|member| member.name.clone()),
        );
        methods.extend(classifier.declared_callable_order.iter().cloned());
        pending.extend(crate::symbol_resolver::direct_supertypes(source, current));
    }
    AccessorInventory::from_methods(methods.iter().map(String::as_str))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accessor_spellings_declare_their_kotlin_property() {
        assert_eq!(accessor_property_name("getUrl").as_deref(), Some("url"));
        assert_eq!(accessor_property_name("getID").as_deref(), Some("id"));
        assert_eq!(
            accessor_property_name("getURLPath").as_deref(),
            Some("urlPath")
        );
        assert_eq!(
            accessor_property_name("getIsInstanceType").as_deref(),
            Some("isInstanceType")
        );
        assert_eq!(accessor_property_name("isOpen").as_deref(), Some("isOpen"));
        assert_eq!(accessor_property_name("getisLower"), None);
        assert_eq!(accessor_property_name("island"), None);
        assert_eq!(accessor_property_name("get"), None);
        assert_eq!(accessor_property_name("is"), None);
    }

    #[test]
    fn setter_is_named_from_the_getter_spelling() {
        assert_eq!(setter_name_for_getter("getUrl"), "setUrl");
        assert_eq!(setter_name_for_getter("isOpen"), "setOpen");
        assert_eq!(setter_name_for_getter("getIsOpen"), "setIsOpen");
        assert_eq!(setter_name_for_getter("getURLPath"), "setURLPath");
    }

    #[test]
    fn inventory_lists_the_is_getter_before_get_spellings() {
        let inventory = AccessorInventory::from_methods([
            "getURLPath",
            "getIsBoth",
            "toString",
            "getUrlPath",
            "isBoth",
        ]);
        assert_eq!(inventory.getters("isBoth"), ["isBoth", "getIsBoth"]);
        assert_eq!(inventory.getters("urlPath"), ["getUrlPath", "getURLPath"]);
        assert!(inventory.getters("both").is_empty());
        assert!(inventory.getters("toString").is_empty());
    }
}
