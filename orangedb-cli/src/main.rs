//! `orangedb`: inspects an orangedb file without the schema's Rust types,
//! from what the database records in its catalog.

mod decode;
mod raw;

use std::collections::BTreeMap;
use std::error::Error;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use orangedb::catalog::{Catalog, Index, Indexed, Layout, Shape};
use orangedb::redb::{MultimapTableHandle, ReadOnlyDatabase, ReadTransaction, ReadableDatabase, TableHandle};

use crate::decode::Rows;

/// Inspect an orangedb database: the schema source behind each layout, its
/// tables, references, indexes and rows.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// The redb file.
    db: PathBuf,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Each entity at its current layout: id, rows, references and
    /// indexes. The default.
    Entities,
    /// The schema source recorded for each layout.
    Source {
        /// An entity, or `Entity@fingerprint` for one layout. Every current
        /// entity if left out.
        entity: Option<String>,
        /// Also layouts the database was migrated away from.
        #[arg(long)]
        all: bool,
    },
    /// Every layout the database has recorded, current or migrated away
    /// from.
    History,
    /// Every redb table in the file: kind, key and value types, entries and
    /// what it holds.
    Tables,
    /// Rows per entity.
    Count {
        /// Entities to count. All of them if left out.
        entities: Vec<String>,
    },
    /// Foreign keys between entities, with their `on_delete` rule and index.
    Refs {
        /// Only references from or to this entity.
        entity: Option<String>,
    },
    /// Foreign key, unique and plain index tables, with their entries.
    Indexes {
        /// Only indexes of this entity.
        entity: Option<String>,
    },
    /// Entries of a table, decoded as far as the catalog's types allow.
    Dump {
        /// An entity (its rows), `Entity@fingerprint` (rows of that layout),
        /// `Entity.field` (a reference's or index's table) or any table's
        /// name.
        target: String,
        /// Show at most this many entries.
        #[arg(long, short = 'n', default_value_t = 20)]
        limit: usize,
        /// Don't shorten long strings and byte strings.
        #[arg(long)]
        full: bool,
    },
}

type Result<T, E = Box<dyn Error>> = std::result::Result<T, E>;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let db = ReadOnlyDatabase::open(&cli.db).map_err(|e| format!("can't open {}: {e}", cli.db.display()))?;
    let tx = db.begin_read()?;
    let catalog = Catalog::read(&tx)?;
    let ctx = Ctx { tx, catalog };
    match cli.command.unwrap_or(Command::Entities) {
        Command::Entities => ctx.entities(),
        Command::Source { entity, all } => ctx.source(entity.as_deref(), all),
        Command::History => ctx.history(),
        Command::Tables => ctx.tables(),
        Command::Count { entities } => ctx.count(&entities),
        Command::Refs { entity } => ctx.refs(entity.as_deref()),
        Command::Indexes { entity } => ctx.indexes(entity.as_deref()),
        Command::Dump { target, limit, full } => ctx.dump(&target, limit, full),
    }
}

struct Ctx {
    tx: ReadTransaction,
    catalog: Catalog,
}

/// Prints rows as columns, the first row being the header.
fn print_table(rows: &[Vec<String>]) {
    let mut widths = Vec::new();
    for row in rows {
        widths.resize(widths.len().max(row.len()), 0);
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    for row in rows {
        let mut line = String::new();
        for (i, cell) in row.iter().enumerate() {
            if i + 1 == row.len() {
                line.push_str(cell);
            } else {
                line.push_str(&format!("{cell:<width$}  ", width = widths[i]));
            }
        }
        println!("{}", line.trim_end());
    }
}

fn header(cells: &[&str]) -> Vec<String> {
    cells.iter().map(|c| c.to_string()).collect()
}

fn fp(fingerprint: u64) -> String {
    format!("{fingerprint:016x}")
}

/// `unique String`, `index Option<u32>`, `index char = #[index(...)]`.
fn index_kind(index: &Index) -> String {
    let kind = if index.unique { "unique" } else { "index" };
    match &index.on {
        Indexed::Field { shape, .. } => format!("{kind} {}", shaped(*shape, &index.ty)),
        Indexed::Expr(written) => format!("{kind} {} = {written}", index.ty),
    }
}

fn shaped(shape: Shape, target: &str) -> String {
    match shape {
        Shape::One => target.to_string(),
        Shape::Option => format!("Option<{target}>"),
        Shape::Vec => format!("Vec<{target}>"),
    }
}

/// The name of the first module the source declares, `mod name`.
fn module_name(source: &str) -> Option<&str> {
    let start = source.find("mod ")? + 4;
    let rest = source[start..].trim_start();
    let end = rest.find(|c: char| !(c.is_alphanumeric() || c == '_'))?;
    Some(&rest[..end])
}

impl Ctx {
    /// The current layouts, or a hint about why there are none.
    fn current(&self) -> Result<Vec<&Layout>> {
        let layouts: Vec<&Layout> = self.catalog.current_layouts().collect();
        if layouts.is_empty() {
            return Err(if self.catalog.current.is_empty() {
                "no entities: this isn't an orangedb database, or one no schema has opened yet \
                 (`tables` and `dump` still work)"
                    .into()
            } else {
                "the database has entities but no recorded layouts: it was last opened by an orangedb \
                 that didn't record them, and is described once a newer one opens it"
                    .into()
            });
        }
        Ok(layouts)
    }

    /// The layout `name` refers to: `Entity` for its current layout, or
    /// `Entity@fingerprint`.
    fn layout(&self, name: &str) -> Result<&Layout> {
        let found = match name.split_once('@') {
            Some((entity, fingerprint)) => {
                let fingerprint = u64::from_str_radix(fingerprint, 16)
                    .map_err(|_| format!("`{fingerprint}` isn't a fingerprint: expected 16 hex digits"))?;
                self.catalog.layout(entity, fingerprint)
            }
            None => self.catalog.current_layouts().find(|l| l.entity == name),
        };
        found.ok_or_else(|| {
            let known: Vec<&str> = self.catalog.current.keys().map(String::as_str).collect();
            format!("no entity layout `{name}`; the entities are: {}", known.join(", ")).into()
        })
    }

    fn len(&self, table: &str, multimap: bool) -> Result<u64> {
        Ok(raw::stat(&self.tx, table, multimap)?.len)
    }

    fn entities(&self) -> Result<()> {
        for (i, layout) in self.current()?.into_iter().enumerate() {
            if i > 0 {
                println!();
            }
            let rows = self.len(&layout.rows, false)?;
            println!(
                "{} @{}: {rows} row{}",
                layout.entity,
                fp(layout.fingerprint),
                if rows == 1 { "" } else { "s" }
            );
            let mut lines = Vec::new();
            let next = match self.catalog.next_ids.get(&layout.entity) {
                Some(next) => format!(", next {next}"),
                None => String::new(),
            };
            lines.push(vec![
                format!("  {}", layout.id.field),
                format!("id {}{next}", layout.id.kind),
                layout.rows.clone(),
            ]);
            for r in &layout.references {
                lines.push(vec![
                    format!("  {}", r.field),
                    format!("-> {} on_delete = {}", shaped(r.shape, &r.target), r.on_delete),
                    r.index.clone(),
                ]);
            }
            for i in &layout.indexes {
                lines.push(vec![format!("  {}", i.name), index_kind(i), i.table.clone()]);
            }
            print_table(&lines);
        }
        Ok(())
    }

    fn source(&self, entity: Option<&str>, all: bool) -> Result<()> {
        let layouts: Vec<&Layout> = match entity {
            Some(name) if all && !name.contains('@') => {
                let layouts: Vec<&Layout> = self.catalog.layouts.iter().filter(|l| l.entity == name).collect();
                if layouts.is_empty() {
                    self.layout(name)?;
                }
                layouts
            }
            Some(name) => vec![self.layout(name)?],
            None if all => self.catalog.layouts.iter().collect(),
            None => self.current()?,
        };
        // Layouts of one module share its source: print it once.
        let mut by_source: BTreeMap<u64, Vec<&Layout>> = BTreeMap::new();
        for layout in layouts {
            by_source.entry(layout.source).or_default().push(layout);
        }
        let mut by_source: Vec<_> = by_source.into_values().collect();
        // Current schemas last, where the terminal leaves them.
        by_source.sort_by_key(|ls| ls.iter().filter(|l| self.catalog.is_current(l)).count());
        for (i, layouts) in by_source.iter().enumerate() {
            if i > 0 {
                println!();
            }
            for layout in layouts {
                let status = if self.catalog.is_current(layout) {
                    "current"
                } else {
                    "migrated"
                };
                println!("// {}@{} ({status})", layout.entity, fp(layout.fingerprint));
            }
            match self.catalog.source(layouts[0]) {
                Some(source) => println!("{source}"),
                None => println!("// (no source recorded)"),
            }
        }
        Ok(())
    }

    fn history(&self) -> Result<()> {
        if self.catalog.layouts.is_empty() {
            self.current()?;
        }
        let mut rows = vec![header(&["ENTITY", "FINGERPRINT", "STATUS", "DECLARED IN"])];
        for layout in &self.catalog.layouts {
            let status = if self.catalog.is_current(layout) {
                "current"
            } else {
                "migrated"
            };
            let module = match self.catalog.source(layout) {
                Some(source) => format!("mod {}", module_name(source).unwrap_or("?")),
                None => "?".into(),
            };
            rows.push(vec![
                layout.entity.clone(),
                fp(layout.fingerprint),
                status.into(),
                format!("{module} (source {:08x})", layout.source >> 32),
            ]);
        }
        print_table(&rows);
        Ok(())
    }

    /// What the table `name` holds, according to the catalog.
    fn role(&self, name: &str) -> String {
        if name.starts_with("__orangedb.") {
            return "orangedb metadata".into();
        }
        for layout in &self.catalog.layouts {
            let stale = if self.catalog.is_current(layout) {
                ""
            } else {
                " (migrated layout)"
            };
            if layout.rows == name {
                return format!("{} rows{stale}", layout.entity);
            }
            for r in &layout.references {
                if r.index == name {
                    return format!("{}.{} -> {} index{stale}", layout.entity, r.field, r.target);
                }
            }
            for i in &layout.indexes {
                if i.table == name {
                    let kind = if i.unique { "unique index" } else { "index" };
                    return format!("{}.{} {kind}{stale}", layout.entity, i.name);
                }
            }
        }
        "-".into()
    }

    fn tables(&self) -> Result<()> {
        let mut rows = vec![header(&["TABLE", "KIND", "KEY", "VALUE", "ENTRIES", "HOLDS"])];
        let tables = self.tx.list_tables()?.map(|t| (t.name().to_string(), false));
        let multimaps = self.tx.list_multimap_tables()?.map(|t| (t.name().to_string(), true));
        let mut all: Vec<(String, bool)> = tables.chain(multimaps).collect();
        all.sort();
        for (name, multimap) in all {
            let stat = raw::stat(&self.tx, &name, multimap)?;
            rows.push(vec![
                name.clone(),
                if multimap { "multimap" } else { "table" }.into(),
                stat.key.name.name().into(),
                stat.value.name.name().into(),
                stat.len.to_string(),
                self.role(&name),
            ]);
        }
        print_table(&rows);
        Ok(())
    }

    fn count(&self, entities: &[String]) -> Result<()> {
        let layouts = if entities.is_empty() {
            self.current()?
        } else {
            entities.iter().map(|e| self.layout(e)).collect::<Result<_>>()?
        };
        let mut rows = Vec::new();
        for layout in layouts {
            rows.push(vec![layout.entity.clone(), self.len(&layout.rows, false)?.to_string()]);
        }
        print_table(&rows);
        Ok(())
    }

    fn refs(&self, entity: Option<&str>) -> Result<()> {
        if let Some(entity) = entity {
            self.layout(entity)?;
        }
        let mut rows = vec![header(&["FROM", "TO", "ON DELETE", "LINKS", "INDEX"])];
        for layout in self.current()? {
            for r in &layout.references {
                if entity.is_some_and(|e| e != layout.entity && e != r.target) {
                    continue;
                }
                rows.push(vec![
                    format!("{}.{}", layout.entity, r.field),
                    shaped(r.shape, &r.target),
                    r.on_delete.to_string(),
                    self.len(&r.index, true)?.to_string(),
                    r.index.clone(),
                ]);
            }
        }
        if rows.len() == 1 {
            println!("no references");
        } else {
            print_table(&rows);
        }
        Ok(())
    }

    fn indexes(&self, entity: Option<&str>) -> Result<()> {
        let layouts = match entity {
            Some(entity) => vec![self.layout(entity)?],
            None => self.current()?,
        };
        let mut rows = vec![header(&["INDEX", "KIND", "NAME", "ENTRIES"])];
        for layout in layouts {
            for r in &layout.references {
                rows.push(vec![
                    r.index.clone(),
                    format!("foreign key -> {}", r.target),
                    format!("{}.{}", layout.entity, r.field),
                    self.len(&r.index, true)?.to_string(),
                ]);
            }
            for i in &layout.indexes {
                rows.push(vec![
                    i.table.clone(),
                    index_kind(i),
                    format!("{}.{}", layout.entity, i.name),
                    self.len(&i.table, !i.unique)?.to_string(),
                ]);
            }
        }
        if rows.len() == 1 {
            println!("no indexes");
        } else {
            print_table(&rows);
        }
        Ok(())
    }

    /// The table `target` names, whether it's a multimap, and the layout
    /// whose rows it holds, if it does.
    fn resolve(&self, target: &str) -> Result<(String, bool, Option<&Layout>)> {
        let rows_of = |name: &str| self.catalog.layouts.iter().find(|l| l.rows == name);
        if self.tx.list_tables()?.any(|t| t.name() == target) {
            return Ok((target.to_string(), false, rows_of(target)));
        }
        if self.tx.list_multimap_tables()?.any(|t| t.name() == target) {
            return Ok((target.to_string(), true, None));
        }
        if let Some((entity, field)) = target.split_once('.') {
            let layout = self.layout(entity)?;
            if let Some(r) = layout.references.iter().find(|r| r.field == field) {
                return Ok((r.index.clone(), true, None));
            }
            if let Some(i) = layout.indexes.iter().find(|i| i.name == field) {
                return Ok((i.table.clone(), !i.unique, None));
            }
            return Err(format!("{entity}.{field} is neither a reference nor an index").into());
        }
        let layout = self.layout(target)?;
        if !self.catalog.is_current(layout) {
            return Err(format!(
                "the database was migrated away from {target}, and its tables were dropped; \
                 see `source {target}` for what it looked like"
            )
            .into());
        }
        Ok((layout.rows.clone(), false, Some(layout)))
    }

    fn dump(&self, target: &str, limit: usize, full: bool) -> Result<()> {
        let (table, multimap, layout) = self.resolve(target)?;
        let dump = raw::entries(&self.tx, &table, multimap, limit).map_err(|e| format!("can't read {table}: {e}"))?;
        let key_ty = dump.stat.key.name.name();
        let value_ty = dump.stat.value.name.name();
        let rows = layout.map(|l| Rows::new(&self.catalog, l, full));
        let mut lines = Vec::new();
        for (key, values) in &dump.entries {
            let key = decode::redb(key_ty, key, &self.catalog, full);
            let values: Vec<String> = values
                .iter()
                .map(|v| match &rows {
                    Some(rows) => rows.row(v),
                    None => decode::redb(value_ty, v, &self.catalog, full),
                })
                .collect();
            let values = if multimap {
                format!("[{}]", values.join(", "))
            } else {
                values.join(", ")
            };
            lines.push(vec![key, values]);
        }
        println!("{table} ({key_ty} -> {value_ty})");
        print_table(&lines);
        let shown: u64 = dump.entries.iter().map(|(_, v)| v.len() as u64).sum();
        if shown < dump.stat.len {
            println!(
                "… showing {shown} of {} entries; pass --limit to see more",
                dump.stat.len
            );
        }
        Ok(())
    }
}
