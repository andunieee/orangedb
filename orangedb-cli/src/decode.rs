//! Turning stored bytes back into text, as far as the recorded type names
//! allow.
//!
//! Keys and index values are redb's own encodings of the type redb records
//! for the table. Rows are postcard, which writes no type information, so
//! they are decoded field by field with the types the catalog records, up
//! to the first type this module doesn't know, such as an `#[embed]` one.

use std::fmt::Write as _;

use orangedb::catalog::{Catalog, IdKind, Layout};

/// A type as written in Rust source, without spaces.
#[derive(Debug, Clone, PartialEq)]
enum Ty {
    /// The last path segment and its generic arguments.
    Path(String, Vec<Ty>),
    Tuple(Vec<Ty>),
    Array(Box<Ty>, usize),
    Slice(Box<Ty>),
    Ref(Box<Ty>),
}

fn parse(text: &str) -> Option<Ty> {
    let (ty, rest) = parse_ty(text)?;
    rest.is_empty().then_some(ty)
}

fn parse_ty(s: &str) -> Option<(Ty, &str)> {
    if let Some(rest) = s.strip_prefix('&') {
        let rest = rest.strip_prefix("mut").unwrap_or(rest);
        let (ty, rest) = parse_ty(rest)?;
        return Some((Ty::Ref(Box::new(ty)), rest));
    }
    if let Some(rest) = s.strip_prefix('(') {
        let (items, rest) = parse_list(rest, ')')?;
        return Some((Ty::Tuple(items), rest));
    }
    if let Some(rest) = s.strip_prefix('[') {
        let (elem, rest) = parse_ty(rest)?;
        if let Some(rest) = rest.strip_prefix(']') {
            return Some((Ty::Slice(Box::new(elem)), rest));
        }
        let rest = rest.strip_prefix(';')?;
        let end = rest.find(']')?;
        let len = rest[..end].parse().ok()?;
        return Some((Ty::Array(Box::new(elem), len), &rest[end + 1..]));
    }
    let end = s
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':'))
        .unwrap_or(s.len());
    let path = &s[..end];
    let name = path.rsplit("::").next()?;
    if name.is_empty() {
        return None;
    }
    let rest = &s[end..];
    match rest.strip_prefix('<') {
        Some(rest) => {
            let (args, rest) = parse_list(rest, '>')?;
            Some((Ty::Path(name.to_string(), args), rest))
        }
        None => Some((Ty::Path(name.to_string(), Vec::new()), rest)),
    }
}

/// Comma-separated types up to `close`, which may follow a trailing comma.
fn parse_list(mut s: &str, close: char) -> Option<(Vec<Ty>, &str)> {
    let mut items = Vec::new();
    loop {
        if let Some(rest) = s.strip_prefix(close) {
            return Some((items, rest));
        }
        let (ty, rest) = parse_ty(s)?;
        items.push(ty);
        s = rest.strip_prefix(',').unwrap_or(rest);
        if !rest.starts_with(',') && !rest.starts_with(close) {
            return None;
        }
    }
}

pub fn hex(bytes: &[u8], full: bool) -> String {
    const SHOWN: usize = 32;
    let shown = if full { bytes } else { &bytes[..bytes.len().min(SHOWN)] };
    let mut out = String::from("0x");
    for b in shown {
        let _ = write!(out, "{b:02x}");
    }
    if shown.len() < bytes.len() {
        let _ = write!(out, "… ({} bytes)", bytes.len());
    }
    out
}

fn string(s: &str, full: bool) -> String {
    const SHOWN: usize = 80;
    match s.char_indices().nth(SHOWN) {
        Some((end, _)) if !full => format!("{:?}… ({} bytes)", &s[..end], s.len()),
        _ => format!("{s:?}"),
    }
}

/// The id kind of the entity whose id newtype is `ty`, preferring layouts
/// declared by the module that declared `near`, then current ones.
fn id_kind<'c>(catalog: &'c Catalog, near: Option<&Layout>, matches: impl Fn(&Layout) -> bool) -> Option<&'c IdKind> {
    let candidates = || catalog.layouts.iter().filter(|l| matches(l));
    candidates()
        .find(|l| near.is_some_and(|n| n.source == l.source))
        .or_else(|| candidates().find(|l| catalog.is_current(l)))
        .or_else(|| candidates().next())
        .map(|l| &l.id.kind)
}

/// A redb key or value, given the type name redb stores for it.
pub fn redb(ty: &str, bytes: &[u8], catalog: &Catalog, full: bool) -> String {
    redb_value(ty, bytes, catalog, full).unwrap_or_else(|| hex(bytes, full))
}

fn redb_value(ty: &str, bytes: &[u8], catalog: &Catalog, full: bool) -> Option<String> {
    macro_rules! int {
        ($($t:ident)*) => {
            match ty {
                $( stringify!($t) => return Some($t::from_le_bytes(bytes.try_into().ok()?).to_string()), )*
                _ => {}
            }
        };
    }
    int!(u8 u16 u32 u64 u128 i8 i16 i32 i64 i128);
    // `orangedb::Entity.id`: an id, stored as the key it wraps. Older files
    // name the id field instead of `id`, which doesn't matter here.
    if let Some((entity, _)) = ty.strip_prefix("orangedb::").and_then(|t| t.split_once('.')) {
        let kind = id_kind(catalog, None, |l| l.entity == entity)?;
        return match kind {
            IdKind::Auto => redb_value("u64", bytes, catalog, full),
            IdKind::Key(ty) => redb_value(ty, bytes, catalog, full),
        };
    }
    if let Some(inner) = ty.strip_prefix("Option<").and_then(|t| t.strip_suffix('>')) {
        return match bytes.split_first()? {
            (0, _) => Some("None".into()),
            (_, rest) => Some(format!("Some({})", redb(inner, rest, catalog, full))),
        };
    }
    match ty {
        "()" => Some("()".into()),
        "bool" => Some((bytes.first()? != &0).to_string()),
        "char" => {
            let [a, b, c] = bytes.try_into().ok()?;
            Some(format!("{:?}", char::from_u32(u32::from_le_bytes([a, b, c, 0]))?))
        }
        "&str" | "String" => Some(string(std::str::from_utf8(bytes).ok()?, full)),
        _ => None,
    }
}

/// Why a row's decoding stopped before its end.
enum Stop {
    /// A type this module can't decode.
    Unknown(String),
    /// Bytes that don't fit the recorded types.
    Malformed,
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Stop> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.bytes.len())
            .ok_or(Stop::Malformed)?;
        let out = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn byte(&mut self) -> Result<u8, Stop> {
        Ok(self.take(1)?[0])
    }

    fn varint(&mut self) -> Result<u128, Stop> {
        let mut out = 0u128;
        for shift in (0..128).step_by(7) {
            let b = self.byte()?;
            out |= u128::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(out);
            }
        }
        Err(Stop::Malformed)
    }

    fn zigzag(&mut self) -> Result<i128, Stop> {
        let n = self.varint()?;
        Ok((n >> 1) as i128 ^ -((n & 1) as i128))
    }

    fn len(&mut self) -> Result<usize, Stop> {
        usize::try_from(self.varint()?).map_err(|_| Stop::Malformed)
    }

    fn str(&mut self) -> Result<&'a str, Stop> {
        let len = self.len()?;
        std::str::from_utf8(self.take(len)?).map_err(|_| Stop::Malformed)
    }
}

/// Decodes rows of one layout.
pub struct Rows<'c> {
    catalog: &'c Catalog,
    layout: &'c Layout,
    full: bool,
}

impl<'c> Rows<'c> {
    pub fn new(catalog: &'c Catalog, layout: &'c Layout, full: bool) -> Self {
        Rows { catalog, layout, full }
    }

    /// `{ field: value, ... }`, with the rest of the row in hex from the
    /// first field that can't be decoded.
    pub fn row(&self, bytes: &[u8]) -> String {
        let mut r = Reader { bytes, pos: 0 };
        let mut parts = Vec::new();
        for field in &self.layout.fields {
            let start = r.pos;
            let ty = field
                .ty
                .as_deref()
                .map(|t| parse(t).ok_or_else(|| Stop::Unknown(t.to_string())));
            let value = match ty {
                None => Err(Stop::Unknown("custom format".into())),
                Some(Err(stop)) => Err(stop),
                Some(Ok(ty)) => self.value(&mut r, &ty),
            };
            match value {
                Ok(value) => parts.push(format!("{}: {value}", field.name)),
                Err(Stop::Unknown(ty)) => {
                    parts.push(format!("{}: ?<{ty}>", field.name));
                    parts.push(format!("… {}", hex(&bytes[start..], self.full)));
                    return format!("{{ {} }}", parts.join(", "));
                }
                Err(Stop::Malformed) => {
                    return format!("{} (doesn't decode as {})", hex(bytes, self.full), self.layout.entity);
                }
            }
        }
        if r.pos < bytes.len() {
            parts.push(format!("+{} unexpected bytes", bytes.len() - r.pos));
        }
        format!("{{ {} }}", parts.join(", "))
    }

    fn values(&self, r: &mut Reader, ty: &Ty, n: usize) -> Result<Vec<String>, Stop> {
        (0..n).map(|_| self.value(r, ty)).collect()
    }

    fn value(&self, r: &mut Reader, ty: &Ty) -> Result<String, Stop> {
        let (name, args) = match ty {
            Ty::Tuple(items) if items.is_empty() => return Ok("()".into()),
            Ty::Tuple(items) => {
                let values: Vec<String> = items.iter().map(|t| self.value(r, t)).collect::<Result<_, _>>()?;
                return Ok(format!("({})", values.join(", ")));
            }
            Ty::Array(elem, n) => return Ok(format!("[{}]", self.values(r, elem, *n)?.join(", "))),
            Ty::Slice(elem) => {
                let n = r.len()?;
                return Ok(format!("[{}]", self.values(r, elem, n)?.join(", ")));
            }
            Ty::Ref(inner) => return self.value(r, inner),
            Ty::Path(name, args) => (name.as_str(), args.as_slice()),
        };
        Ok(match (name, args) {
            ("bool", []) => match r.byte()? {
                0 => "false".into(),
                1 => "true".into(),
                _ => return Err(Stop::Malformed),
            },
            ("u8", []) => r.byte()?.to_string(),
            ("i8", []) => (r.byte()? as i8).to_string(),
            ("u16" | "u32" | "u64" | "u128" | "usize", []) => r.varint()?.to_string(),
            ("i16" | "i32" | "i64" | "i128" | "isize", []) => r.zigzag()?.to_string(),
            ("f32", []) => f32::from_le_bytes(r.take(4)?.try_into().unwrap()).to_string(),
            ("f64", []) => f64::from_le_bytes(r.take(8)?.try_into().unwrap()).to_string(),
            ("char", []) => format!("{:?}", r.str()?.chars().next().ok_or(Stop::Malformed)?),
            ("String" | "str", []) => string(r.str()?, self.full),
            ("Duration", []) => format!("{}.{:09}s", r.varint()?, r.varint()?),
            ("SystemTime", []) => format!("{}.{:09}s after the epoch", r.varint()?, r.varint()?),
            ("PhantomData", _) => "PhantomData".into(),
            ("Option", [inner]) => match r.byte()? {
                0 => "None".into(),
                1 => format!("Some({})", self.value(r, inner)?),
                _ => return Err(Stop::Malformed),
            },
            ("Box" | "Rc" | "Arc" | "Cow", [inner]) => self.value(r, inner)?,
            ("Vec" | "VecDeque" | "LinkedList" | "HashSet" | "BTreeSet" | "BinaryHeap", [elem]) => {
                let n = r.len()?;
                format!("[{}]", self.values(r, elem, n)?.join(", "))
            }
            ("HashMap" | "BTreeMap", [k, v]) => {
                let n = r.len()?;
                let mut pairs = Vec::with_capacity(n.min(1024));
                for _ in 0..n {
                    pairs.push(format!("{}: {}", self.value(r, k)?, self.value(r, v)?));
                }
                format!("{{{}}}", pairs.join(", "))
            }
            (name, []) => {
                let kind = id_kind(self.catalog, Some(self.layout), |l| l.id.ty == name)
                    .ok_or_else(|| Stop::Unknown(name.to_string()))?;
                match kind {
                    IdKind::Auto => r.varint()?.to_string(),
                    IdKind::Key(key) => self.value(r, &parse(key).ok_or_else(|| Stop::Unknown(key.clone()))?)?,
                }
            }
            _ => return Err(Stop::Unknown(type_text(ty))),
        })
    }
}

fn type_text(ty: &Ty) -> String {
    let list = |items: &[Ty]| items.iter().map(type_text).collect::<Vec<_>>().join(", ");
    match ty {
        Ty::Path(name, args) if args.is_empty() => name.clone(),
        Ty::Path(name, args) => format!("{name}<{}>", list(args)),
        Ty::Tuple(items) => format!("({})", list(items)),
        Ty::Array(elem, n) => format!("[{}; {n}]", type_text(elem)),
        Ty::Slice(elem) => format!("[{}]", type_text(elem)),
        Ty::Ref(inner) => format!("&{}", type_text(inner)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(name: &str, args: Vec<Ty>) -> Ty {
        Ty::Path(name.into(), args)
    }

    #[test]
    fn parses_types_as_written() {
        assert_eq!(parse("String"), Some(path("String", vec![])));
        assert_eq!(
            parse("std::collections::BTreeMap<String,Vec<u8>>"),
            Some(path(
                "BTreeMap",
                vec![path("String", vec![]), path("Vec", vec![path("u8", vec![])])]
            ))
        );
        assert_eq!(
            parse("(u32,bool,)"),
            Some(Ty::Tuple(vec![path("u32", vec![]), path("bool", vec![])]))
        );
        assert_eq!(parse("[u8;4]"), Some(Ty::Array(Box::new(path("u8", vec![])), 4)));
        assert_eq!(parse("()"), Some(Ty::Tuple(vec![])));
        assert_eq!(parse("Vec<u8"), None);
    }

    #[test]
    fn reads_postcard_integers() {
        let mut r = Reader {
            bytes: &[0xac, 0x02, 0x03],
            pos: 0,
        };
        assert_eq!(r.varint().ok(), Some(300));
        assert_eq!(r.zigzag().ok(), Some(-2));
    }
}
