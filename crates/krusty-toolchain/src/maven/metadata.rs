//! Metadata read from the store once per run: POM and Gradle module files, parsed POMs and module
//! variants, and effective POMs. Every module's resolution reads through one `Metadata`, so an
//! artifact many modules depend on is parsed once.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::effective_pom::{EffectivePom, Reading};
use super::gradle_module::{self, Variant};
use super::pom::{self, Pom};
use super::{Coordinates, Store};

pub struct Metadata<'s> {
    store: &'s Store,
    /// File texts by coordinates and extension.
    files: Cache<(Coordinates, &'static str), Option<Rc<str>>>,
    poms: Cache<Coordinates, Result<Rc<Pom>, String>>,
    modules: Cache<Coordinates, Result<Rc<[Variant]>, String>>,
    /// Effective POMs by their coordinates and the JDK version their profiles were judged for.
    effective: Cache<EffectiveKey, Result<Rc<EffectivePom>, Vec<String>>>,
    /// The effective POMs being read, which a POM importing itself must read anew.
    reading: RefCell<HashSet<EffectiveKey>>,
}

type Cache<K, V> = RefCell<HashMap<K, V>>;

type EffectiveKey = (Coordinates, String);

impl<'s> Metadata<'s> {
    pub fn new(store: &'s Store) -> Self {
        Self {
            store,
            files: RefCell::default(),
            poms: RefCell::default(),
            modules: RefCell::default(),
            effective: RefCell::default(),
            reading: RefCell::default(),
        }
    }

    pub fn store(&self) -> &'s Store {
        self.store
    }

    /// The text of the file of `coordinates` with `extension`, when it is in the store.
    pub fn file(&self, coordinates: &Coordinates, extension: &'static str) -> Option<Rc<str>> {
        let key = (coordinates.clone(), extension);
        if let Some(text) = self.files.borrow().get(&key) {
            return text.clone();
        }
        let text = self.store.read(coordinates, extension).map(Rc::from);
        self.files.borrow_mut().insert(key, text.clone());
        text
    }

    /// The raw POM published under `coordinates`, or why it cannot be read.
    pub fn pom(&self, coordinates: &Coordinates) -> Result<Rc<Pom>, String> {
        if let Some(pom) = self.poms.borrow().get(coordinates) {
            return pom.clone();
        }
        let pom = match self.file(coordinates, "pom") {
            None => Err(format!(
                "The POM of {} is not in the cache",
                coordinates.pretty(None)
            )),
            Some(text) => pom::parse(&text, &coordinates.group, &coordinates.artifact)
                .map(Rc::new)
                .map_err(|problem| {
                    format!(
                        "Unable to parse the POM of {}: {problem}",
                        coordinates.pretty(None)
                    )
                }),
        };
        self.poms
            .borrow_mut()
            .insert(coordinates.clone(), pom.clone());
        pom
    }

    /// The variants of the Gradle module metadata `text` published under `coordinates`.
    pub fn variants(&self, coordinates: &Coordinates, text: &str) -> Result<Rc<[Variant]>, String> {
        if let Some(variants) = self.modules.borrow().get(coordinates) {
            return variants.clone();
        }
        let variants = gradle_module::parse(text).map(Rc::from);
        self.modules
            .borrow_mut()
            .insert(coordinates.clone(), variants.clone());
        variants
    }

    /// The effective POM published under `coordinates`, its profiles judged for `jdk_version`.
    pub fn effective_pom(
        &self,
        coordinates: &Coordinates,
        jdk_version: &str,
    ) -> Result<Rc<EffectivePom>, Vec<String>> {
        self.shared_effective_pom(coordinates, jdk_version)
            .unwrap_or_else(|| self.read_effective_pom(coordinates, jdk_version))
    }

    /// The effective POM published under `coordinates`, unless it is being read.
    pub(super) fn shared_effective_pom(
        &self,
        coordinates: &Coordinates,
        jdk_version: &str,
    ) -> Option<Result<Rc<EffectivePom>, Vec<String>>> {
        let key = (coordinates.clone(), jdk_version.to_string());
        if let Some(effective) = self.effective.borrow().get(&key) {
            return Some(effective.clone());
        }
        if !self.reading.borrow_mut().insert(key.clone()) {
            return None;
        }
        let effective = self.read_effective_pom(coordinates, jdk_version);
        self.reading.borrow_mut().remove(&key);
        self.effective.borrow_mut().insert(key, effective.clone());
        Some(effective)
    }

    fn read_effective_pom(
        &self,
        coordinates: &Coordinates,
        jdk_version: &str,
    ) -> Result<Rc<EffectivePom>, Vec<String>> {
        self.pom(coordinates)
            .map_err(|problem| vec![problem])
            .map(|raw| {
                let mut reading = Reading::new(self, jdk_version);
                let pom = reading.effective(&raw, 0);
                Rc::new(EffectivePom {
                    pom,
                    problems: reading.problems,
                    height: reading.deepest,
                })
            })
    }
}
