//! Compile-time fingerprints of the stored format of every type a row can
//! contain.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, LinkedList, VecDeque};
use std::hash::Hash;
use std::time::{Duration, SystemTime};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::__private::Sealed;

/// A hash of how a type is stored, computed at compile time.
///
/// Every field of an entity must implement it, which is how a schema refuses
/// types whose stored format orangedb can't see. It is implemented for the
/// std types serde supports and for the id newtypes; declare your
/// own structs and enums with [`#[orangedb::embed]`](crate::embed). It can't
/// be implemented by hand.
///
/// Sequences (`Vec`, `VecDeque`, sets, ...) share a fingerprint, as do maps,
/// because they are stored the same way: switching between them needs no
/// migration.
#[diagnostic::on_unimplemented(
    message = "`{Self}` has no orangedb fingerprint",
    label = "orangedb can't tell how this type is stored",
    note = "declare the type with `#[orangedb::embed]` (or `#[embed]` inside the schema module), \
            or describe the field's stored format yourself with `#[fingerprint = \"...\"]`"
)]
pub trait Fingerprint: Serialize + DeserializeOwned + Sealed {
    const FINGERPRINT: u64;
}

/// FNV-1a, usable in const context.
#[doc(hidden)]
pub struct Fp(u64);

impl Fp {
    pub const fn new(tag: &str) -> Self {
        Fp(0xcbf29ce484222325).str(tag)
    }

    pub const fn str(mut self, s: &str) -> Self {
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            self = self.byte(bytes[i]);
            i += 1;
        }
        self.byte(0xff)
    }

    pub const fn u64(mut self, v: u64) -> Self {
        let bytes = v.to_le_bytes();
        let mut i = 0;
        while i < bytes.len() {
            self = self.byte(bytes[i]);
            i += 1;
        }
        self
    }

    pub const fn finish(self) -> u64 {
        self.0
    }

    const fn byte(self, b: u8) -> Self {
        Fp((self.0 ^ b as u64).wrapping_mul(0x100000001b3))
    }
}

macro_rules! leaf {
    ($($ty:ty => $tag:literal),* $(,)?) => {$(
        impl Sealed for $ty {}
        impl Fingerprint for $ty {
            const FINGERPRINT: u64 = Fp::new($tag).finish();
        }
    )*};
}

leaf!(
    () => "unit",
    bool => "bool",
    char => "char",
    u8 => "u8",
    u16 => "u16",
    u32 => "u32",
    u64 => "u64",
    u128 => "u128",
    i8 => "i8",
    i16 => "i16",
    i32 => "i32",
    i64 => "i64",
    i128 => "i128",
    // serde stores pointer-sized integers as 64-bit ones.
    usize => "u64",
    isize => "i64",
    f32 => "f32",
    f64 => "f64",
    String => "string",
    Duration => "duration",
    SystemTime => "system_time",
);

impl<T> Sealed for Option<T> {}
impl<T: Fingerprint> Fingerprint for Option<T> {
    const FINGERPRINT: u64 = Fp::new("option").u64(T::FINGERPRINT).finish();
}

/// A box is stored as what it holds.
impl<T> Sealed for Box<T> {}
impl<T: Fingerprint> Fingerprint for Box<T> {
    const FINGERPRINT: u64 = T::FINGERPRINT;
}

macro_rules! seq {
    ($($ty:ident $(: $bound:path)*),* $(,)?) => {$(
        impl<T> Sealed for $ty<T> {}
        impl<T: Fingerprint $(+ $bound)*> Fingerprint for $ty<T> {
            const FINGERPRINT: u64 = Fp::new("seq").u64(T::FINGERPRINT).finish();
        }
    )*};
}

seq!(Vec, VecDeque, LinkedList, BTreeSet: Ord, HashSet: Eq: Hash);

macro_rules! map {
    ($($ty:ident $(: $bound:path)*),* $(,)?) => {$(
        impl<K, V> Sealed for $ty<K, V> {}
        impl<K: Fingerprint $(+ $bound)*, V: Fingerprint> Fingerprint for $ty<K, V> {
            const FINGERPRINT: u64 = Fp::new("map").u64(K::FINGERPRINT).u64(V::FINGERPRINT).finish();
        }
    )*};
}

map!(BTreeMap: Ord, HashMap: Eq: Hash);

/// serde implements arrays one length at a time, up to 32.
macro_rules! array {
    ($($n:literal)*) => {$(
        impl<T> Sealed for [T; $n] {}
        impl<T: Fingerprint> Fingerprint for [T; $n] {
            const FINGERPRINT: u64 = Fp::new("array").u64($n).u64(T::FINGERPRINT).finish();
        }
    )*};
}

array!(0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31 32);

macro_rules! tuple {
    ($( ($($t:ident)+) )*) => {$(
        impl<$($t),+> Sealed for ($($t,)+) {}
        impl<$($t: Fingerprint),+> Fingerprint for ($($t,)+) {
            const FINGERPRINT: u64 = Fp::new("tuple") $( .u64($t::FINGERPRINT) )+ .finish();
        }
    )*};
}

tuple! {
    (A)
    (A B)
    (A B C)
    (A B C D)
    (A B C D E)
    (A B C D E F)
    (A B C D E F G)
    (A B C D E F G H)
    (A B C D E F G H I)
    (A B C D E F G H I J)
    (A B C D E F G H I J K)
    (A B C D E F G H I J K L)
}
