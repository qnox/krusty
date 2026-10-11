//! The `$WhenMappings` tables kotlinc's `MappedEnumWhenLowering` gives each class whose code
//! switches on an enum, and the synthetic nested class holding them.
//!
//! A class owns one `int[] $EnumSwitchMapping$N` per enum its `when`s switch on, numbered in the
//! order the class first claims one, and each table maps an entry's runtime ordinal to the key the
//! class first gave that entry, counting from 1 (0 is an unknown ordinal). The class that emits a
//! `when` is a placement fact of this backend, so the tables are filled as code is emitted, in the
//! claim order common lowering recorded on each `when`.

use super::*;
use crate::ir::EnumSwitchRegistration;
use crate::jvm::classfile::InnerClassSpec;

/// `JvmLoweredDeclarationOrigin.ENUM_MAPPINGS_FOR_WHEN` names its class `WhenMappings`.
const WHEN_MAPPINGS: &str = "WhenMappings";
const NO_SUCH_FIELD_ERROR: &str = "java/lang/NoSuchFieldError";
/// `PUBLIC | STATIC | FINAL | SYNTHETIC`, as both sides of the nesting declare it.
const INNER_CLASS_ACCESS: u16 = 0x1019;
const T_INT: u8 = 10;
const IALOAD: u8 = 0x2e;
const IASTORE: u8 = 0x4f;

/// The tables of every class this emission run wrote code for, in first-claim order.
#[derive(Default)]
pub(super) struct WhenMappingTables {
    owners: Vec<(String, Vec<EnumTable>)>,
}

/// One `$EnumSwitchMapping$N`: an enum and its entries in key order.
struct EnumTable {
    classifier: TypeName,
    entries: Vec<Box<str>>,
}

impl WhenMappingTables {
    pub(super) fn clear(&mut self) {
        self.owners.clear();
    }

    fn tables(&mut self, owner: &str) -> &mut Vec<EnumTable> {
        let index = match self.owners.iter().position(|(name, _)| name == owner) {
            Some(index) => index,
            None => {
                self.owners.push((owner.to_string(), Vec::new()));
                self.owners.len() - 1
            }
        };
        &mut self.owners[index].1
    }

    /// kotlinc's `getMappingForClass` and `mapConstEnumEntry`: the table index of the claimed enum
    /// and, for an entry, its key, each assigned on first claim.
    fn claim(&mut self, owner: &str, registration: &EnumSwitchRegistration) -> (usize, i32) {
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
        let Some(entry) = &registration.entry else {
            return (table, 0);
        };
        let entries = &mut tables[table].entries;
        let key = match entries.iter().position(|claimed| claimed == entry) {
            Some(key) => key,
            None => {
                entries.push(entry.clone());
                entries.len() - 1
            }
        };
        // Key 0 is the default of an unfilled slot: an entry this class was compiled without.
        (table, key as i32 + 1)
    }

    /// The `InnerClasses` row an owning class declares for its mappings, once it has any.
    pub(super) fn inner_class(&self, owner: &str) -> Option<InnerClassSpec> {
        self.owners
            .iter()
            .any(|(name, _)| name == owner)
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
            .map(|(owner, tables)| {
                let internal = mappings_class(owner);
                let bytes = write_class(owner, &internal, tables, env, opts);
                (internal, bytes)
            })
            .collect()
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
    owner: &str,
    internal: &str,
    tables: &[EnumTable],
    env: &EmitEnv,
    opts: &EmitOptions,
) -> Vec<u8> {
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
    cw.add_inner_class(InnerClassSpec {
        inner: internal.to_string(),
        outer: Some(owner.to_string()),
        name: Some(WHEN_MAPPINGS.to_string()),
        access: INNER_CLASS_ACCESS,
    });
    metadata_policy::finish_local_synthetic_class(cw, env)
}

impl Emitter<'_> {
    /// `<mapping>[subject.ordinal()]`, after claiming the tables of this `when` and of every
    /// subject `when` nested in it.
    pub(super) fn emit_enum_switch_ordinal(
        &mut self,
        classifier: TypeName,
        subject: ExprId,
        registrations: &[EnumSwitchRegistration],
        code: &mut CodeBuilder,
    ) {
        let mut table = None;
        for registration in registrations {
            let (index, _) = self
                .run
                .when_mappings
                .borrow_mut()
                .claim(&self.owner, registration);
            if registration.classifier == classifier {
                table = Some(index);
            }
        }
        let table = table.expect("a subject when claims its own enum's table");
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
        let registration = EnumSwitchRegistration {
            classifier,
            entry: Some(name.into()),
        };
        self.run
            .when_mappings
            .borrow_mut()
            .claim(&self.owner, &registration)
            .1
    }
}
