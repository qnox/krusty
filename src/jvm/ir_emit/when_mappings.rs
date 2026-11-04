//! The `$WhenMappings` tables kotlinc's `MappedEnumWhenLowering` gives each class whose code
//! switches on an enum, and the synthetic nested class holding them.
//!
//! A class owns one `int[] $EnumSwitchMapping$N` per enum its `when`s switch on, numbered in the
//! order the class first claims one, and each table maps an entry's runtime ordinal to the key the
//! class first gave that entry, counting from 1 (0 is an unknown ordinal). kotlinc claims them as
//! it visits the class's declarations in source order, before any code is generated or inlined.
//! The class that emits a `when` is a placement fact of this backend, so before an emission pass
//! writes any code, each class's tables are numbered from the `when`s placed in it, ordered by the
//! source order common lowering recorded on each. Emission, and the compiled body of a same-module
//! inline function that a caller inlines, then only read those numbers.

use super::static_accessors::EmissionContext;
use super::*;
use crate::ir::{EmittedDeclaration, EnumSwitchRegistration, FunId};
use crate::jvm::classfile::InnerClassSpec;

/// `JvmLoweredDeclarationOrigin.ENUM_MAPPINGS_FOR_WHEN` names its class `WhenMappings`.
const WHEN_MAPPINGS: &str = "WhenMappings";
const NO_SUCH_FIELD_ERROR: &str = "java/lang/NoSuchFieldError";
/// `PUBLIC | STATIC | FINAL | SYNTHETIC`, as both sides of the nesting declare it.
const INNER_CLASS_ACCESS: u16 = 0x1019;
const T_INT: u8 = 10;
const IALOAD: u8 = 0x2e;
const IASTORE: u8 = 0x4f;

/// The tables of every class of this emission pass, in first-claim order.
#[derive(Default)]
pub(super) struct WhenMappingTables {
    owners: Vec<OwnerTables>,
}

/// One class's tables, and whether its mappings class is part of a public inline function's ABI.
struct OwnerTables {
    owner: String,
    tables: Vec<EnumTable>,
    public_abi: bool,
}

/// One `$EnumSwitchMapping$N`: an enum and its entries in key order.
struct EnumTable {
    classifier: TypeName,
    entries: Vec<Box<str>>,
}

/// One subject `when` placed in a class: its order key and the claims it makes.
struct Site<'a> {
    source_order: Option<u32>,
    registrations: &'a [EnumSwitchRegistration],
}

/// Number the tables of every class of this pass from the subject `when`s its code holds, in source
/// order, replacing the previous pass's tables. A foreign inline template is another file's code,
/// whose tables that file numbers. A class whose tables code in a non-private inline function's
/// scope reads marks its mappings class part of that function's ABI, as kotlinc does.
///
/// Every subject `when` lowered from source carries its order key; one without fails the pass.
pub(super) fn seed(ir: &IrFile, facade: &str, contexts: &[EmissionContext], run: &EmitRun) {
    let numbered = run.when_mappings.borrow_mut().number(ir, facade, contexts);
    if let Err(reason) = numbered {
        run.set_emit_error(reason);
    }
}

impl WhenMappingTables {
    fn number(
        &mut self,
        ir: &IrFile,
        facade: &str,
        contexts: &[EmissionContext],
    ) -> Result<(), String> {
        self.owners.clear();
        let roots = code_roots(ir, contexts)?;
        for (context, roots) in contexts.iter().zip(roots) {
            let class_enclosure = context
                .class
                .and_then(|class| ir.classes.get(class))
                .and_then(|class| class.enclosure);
            let mut public_abi =
                super::metadata_policy::in_public_inline_scope(ir, class_enclosure);
            let mut sites = Vec::new();
            for (root, function) in roots {
                let found = sites.len();
                collect_sites(ir, root, &mut sites);
                if sites.len() > found
                    && function.is_some_and(|function| {
                        super::metadata_policy::in_public_inline_scope(
                            ir,
                            Some(crate::ir::IrEnclosure::Function(function)),
                        )
                    })
                {
                    public_abi = true;
                }
            }
            if sites.is_empty() {
                continue;
            }
            let owner = context.owner.internal_name(facade);
            let mut ordered = Vec::with_capacity(sites.len());
            for site in sites {
                let source_order = site.source_order.ok_or_else(|| {
                    format!("a subject `when` of {owner} has no recorded source order")
                })?;
                ordered.push((source_order, site.registrations));
            }
            ordered.sort_by_key(|&(source_order, _)| source_order);
            for registration in ordered
                .iter()
                .flat_map(|(_, registrations)| registrations.iter())
            {
                self.claim(&owner, registration);
            }
            if let Some(tables) = self.owners.iter_mut().find(|tables| tables.owner == owner) {
                tables.public_abi |= public_abi;
            }
        }
        Ok(())
    }

    fn tables(&mut self, owner: &str) -> &mut Vec<EnumTable> {
        let index = match self.owners.iter().position(|tables| tables.owner == owner) {
            Some(index) => index,
            None => {
                self.owners.push(OwnerTables {
                    owner: owner.to_string(),
                    tables: Vec::new(),
                    public_abi: false,
                });
                self.owners.len() - 1
            }
        };
        &mut self.owners[index].tables
    }

    /// kotlinc's `getMappingForClass` and `mapConstEnumEntry`: the table index of the claimed enum
    /// and, for an entry, its key, each assigned on first claim.
    fn claim(&mut self, owner: &str, registration: &EnumSwitchRegistration) {
        let tables = self.tables(owner);
        let table = match tables
            .iter()
            .position(|table| table.classifier == registration.classifier)
        {
            Some(table) => table,
            None => {
                tables.push(EnumTable {
                    classifier: registration.classifier,
                    entries: Vec::new(),
                });
                tables.len() - 1
            }
        };
        if let Some(entry) = &registration.entry {
            let entries = &mut tables[table].entries;
            if !entries.contains(entry) {
                entries.push(entry.clone());
            }
        }
    }

    /// The table index `owner` numbered for `classifier` and, for an `entry`, its key, counting
    /// from 1: key 0 is the default of an unfilled slot, an entry the class was compiled without.
    fn read(&self, owner: &str, classifier: TypeName, entry: Option<&str>) -> Option<(usize, i32)> {
        let tables = &self
            .owners
            .iter()
            .find(|tables| tables.owner == owner)?
            .tables;
        let table = tables
            .iter()
            .position(|table| table.classifier == classifier)?;
        let key = match entry {
            Some(entry) => {
                tables[table]
                    .entries
                    .iter()
                    .position(|claimed| &**claimed == entry)? as i32
                    + 1
            }
            None => 0,
        };
        Some((table, key))
    }

    /// The `InnerClasses` row an owning class declares for its mappings, once it has any.
    pub(super) fn inner_class(&self, owner: &str) -> Option<InnerClassSpec> {
        self.owners
            .iter()
            .any(|tables| tables.owner == owner)
            .then(|| InnerClassSpec {
                inner: mappings_class(owner),
                outer: Some(owner.to_string()),
                name: Some(WHEN_MAPPINGS.to_string()),
                access: INNER_CLASS_ACCESS,
            })
    }

    /// Write the `$WhenMappings` class of every owner, in first-claim order.
    pub(super) fn write_classes(
        &self,
        env: &EmitEnv,
        opts: &EmitOptions,
    ) -> Vec<(String, Vec<u8>)> {
        self.owners
            .iter()
            .map(|tables| {
                let internal = mappings_class(&tables.owner);
                let bytes = write_class(tables, &internal, env, opts);
                (internal, bytes)
            })
            .collect()
    }
}

/// One class's code roots, each with the function whose inline scope it is in.
type ClassCode = Vec<(ExprId, Option<FunId>)>;

/// The code each context's class holds, in `contexts` order: its emission roots, and the defaults
/// its default-argument wrappers evaluate ([`crate::jvm::default_wrappers::evaluated_defaults`]),
/// each with the function whose inline scope it is in. A foreign inline template is another
/// file's code, whose tables that file numbers.
fn code_roots(ir: &IrFile, contexts: &[EmissionContext]) -> Result<Vec<ClassCode>, String> {
    let body_functions = ir
        .functions
        .iter()
        .enumerate()
        .filter_map(|(function, declaration)| Some((declaration.body?, function as FunId)))
        .collect::<HashMap<_, _>>();
    let method_classes = ir
        .classes
        .iter()
        .enumerate()
        .flat_map(|(class, declaration)| declaration.methods.iter().map(move |&m| (m, class)))
        .collect::<HashMap<_, _>>();
    let mut roots = contexts
        .iter()
        .map(|context| {
            context
                .roots
                .iter()
                .map(|&root| (root, body_functions.get(&root).copied()))
                .filter(|(_, function)| {
                    !function
                        .is_some_and(|function| ir.foreign_inline_templates.contains(&function))
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    for (declaration, root) in crate::jvm::default_wrappers::evaluated_defaults(ir) {
        let (class, function) = match declaration {
            EmittedDeclaration::Function(function) => {
                (method_classes.get(&function).copied(), Some(function))
            }
            EmittedDeclaration::PrimaryConstructor(class)
            | EmittedDeclaration::SecondaryConstructor { class, .. } => {
                (Some(class as usize), None)
            }
            EmittedDeclaration::Static(index) => (
                ir.statics[index]
                    .owner
                    .and_then(|owner| ir.classes.iter().position(|c| c.fq_name == owner)),
                None,
            ),
        };
        let context = contexts
            .iter()
            .position(|context| context.class == class)
            .ok_or_else(|| {
                format!("the default-argument wrapper of {declaration:?} has no class")
            })?;
        // A method's default is also an emission-context root, known there without the function
        // whose wrapper evaluates it; the wrapper's declaration is that root's owner.
        match roots[context].iter_mut().find(|(known, _)| *known == root) {
            None => roots[context].push((root, function)),
            Some((_, owner @ None)) => *owner = function,
            Some((_, Some(owner))) if function.is_none_or(|function| function == *owner) => {}
            Some((_, Some(owner))) => {
                return Err(format!(
                    "the default {root} belongs to function {owner} and to {declaration:?}"
                ));
            }
        }
    }
    Ok(roots)
}

/// Every subject `when` in the code under `root`, each once.
fn collect_sites<'a>(ir: &'a IrFile, root: ExprId, sites: &mut Vec<Site<'a>>) {
    let mut seen = HashSet::default();
    let mut pending = vec![root];
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let IrExpr::EnumSwitchOrdinal {
            registrations,
            source_order,
            ..
        } = ir.expr(expression)
        {
            sites.push(Site {
                source_order: *source_order,
                registrations,
            });
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
}

fn mappings_class(owner: &str) -> String {
    format!("{owner}${WHEN_MAPPINGS}")
}

fn table_field(table: usize) -> String {
    format!("$EnumSwitchMapping${table}")
}

/// `final class Owner$WhenMappings` with one `public static final synthetic int[]` per table, each
/// filled in `<clinit>` as kotlinc's field initializer does: sized by `values()`, and every claimed
/// entry stored at its runtime ordinal under a `NoSuchFieldError` guard, so an entry missing from
/// the enum the program runs against leaves its slot unknown (KT-38637).
fn write_class(
    owner_tables: &OwnerTables,
    internal: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
) -> Vec<u8> {
    let OwnerTables {
        owner,
        tables,
        public_abi,
    } = owner_tables;
    let mut cw = new_writer(internal, "java/lang/Object", opts);
    cw.set_access(0x1031); // PUBLIC | FINAL | SUPER | SYNTHETIC
    cw.reserve_method_name("<clinit>");
    cw.reserve_descriptor("()V");
    // ASM visits a method's try-catch blocks before its instructions.
    let no_such_field = cw.class_ref(NO_SUCH_FIELD_ERROR);
    let mut code = CodeBuilder::new(0);
    for (index, table) in tables.iter().enumerate() {
        let classifier = table.classifier.render();
        let values = cw.methodref(&classifier, "values", &format!("()[L{classifier};"));
        code.invokestatic(values, 0, 1);
        code.arraylength();
        code.newarray(T_INT);
        code.astore(0);
        for (key, entry) in table.entries.iter().enumerate() {
            let (start, end, handler, after) = (
                code.new_label(),
                code.new_label(),
                code.new_label(),
                code.new_label(),
            );
            code.bind(start);
            code.nop();
            code.aload(0);
            let field = cw.fieldref(&classifier, entry, &format!("L{classifier};"));
            code.getstatic(field, 1);
            let ordinal = cw.methodref(&classifier, "ordinal", "()I");
            code.invokevirtual(ordinal, 0, 1);
            code.push_int(key as i32 + 1, &mut cw);
            code.array_store(IASTORE, 1);
            code.bind(end);
            code.goto(after);
            code.bind_handler(handler, &[(start, end)]);
            code.set_stack(1);
            code.astore(1);
            code.bind(after);
            code.add_exception(start, end, handler, no_such_field);
        }
        code.aload(0);
        let field = cw.fieldref(internal, &table_field(index), "[I");
        code.putstatic(field, 1);
    }
    code.ret_void();
    code.ensure_locals(2);
    code.link();
    cw.add_method(0x0008, "<clinit>", "()V", &code); // STATIC
    for index in 0..tables.len() {
        cw.add_field(0x1019, &table_field(index), "[I"); // PUBLIC | STATIC | FINAL | SYNTHETIC
    }
    // The owner may itself be nested (an object expression), which the file's nest declares.
    env.inner_classes.register(&mut cw);
    cw.add_inner_class(InnerClassSpec {
        inner: internal.to_string(),
        outer: Some(owner.to_string()),
        name: Some(WHEN_MAPPINGS.to_string()),
        access: INNER_CLASS_ACCESS,
    });
    metadata_policy::finish_local_synthetic_class_in_abi(cw, env, *public_abi)
}

impl Emitter<'_> {
    /// `<mapping>[subject.ordinal()]`, after claiming the tables of this `when` and of every
    /// subject `when` nested in it.
    pub(super) fn emit_enum_switch_ordinal(
        &mut self,
        classifier: TypeName,
        subject: ExprId,
        code: &mut CodeBuilder,
    ) {
        let table = self.numbered_enum_switch(classifier, None).0;
        let field = self
            .cw
            .fieldref(&mappings_class(&self.owner), &table_field(table), "[I");
        code.getstatic(field, 1);
        self.emit_value(subject, code);
        let classifier = classifier.render();
        let ordinal = self.cw.methodref(&classifier, "ordinal", "()I");
        code.invokevirtual(ordinal, 0, 1);
        code.array_load(IALOAD, 1);
    }

    /// The key the emitting class's table gives an entry.
    pub(super) fn enum_switch_key(&self, classifier: TypeName, name: &str) -> i32 {
        self.numbered_enum_switch(classifier, Some(name)).1
    }

    /// What the emitting class numbered for `classifier`'s table and `entry`. Every `when` placed
    /// in the class was numbered before the pass wrote code; a read the numbering lacks fails it.
    fn numbered_enum_switch(&self, classifier: TypeName, entry: Option<&str>) -> (usize, i32) {
        let numbered = self
            .run
            .when_mappings
            .borrow()
            .read(&self.owner, classifier, entry);
        numbered.unwrap_or_else(|| {
            self.run.set_emit_error(format!(
                "{} reads a `$WhenMappings` entry its numbering lacks",
                self.owner
            ));
            (0, 0)
        })
    }
}
