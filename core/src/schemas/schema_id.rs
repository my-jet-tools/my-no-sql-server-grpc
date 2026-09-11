//! The one arithmetic every schema id is made of.
//!
//! An id is folded **once, at compile time**, by the macro: a type's own shape
//! plus the ids of the types its fields carry. Nothing recomputes it afterwards,
//! not the writer and least of all the server, which takes the number the client
//! declared and only ever compares the bytes behind it.
//!
//! The by-bytes fold ([`get_schema_id`]) lives here beside it because it is the
//! same arithmetic and answers the same question about the same input, so "how a
//! schema id is computed" has one answer rather than one per caller. It is not
//! on the serving path.
//!
//! FNV-1a, hand rolled: what is needed is a number which is the same in every
//! process and every build over the same input, and the standard hashers promise
//! neither. Hand rolled also means `const fn`, which is what lets the macro emit
//! `const SCHEMA_ID: u64` rather than a `OnceLock` somebody has to remember to
//! prime.

use super::Scalar;

/// Where FNV-1a starts, published rather than chosen.
pub const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

pub const fn fnv_byte(hash: u64, byte: u8) -> u64 {
    (hash ^ byte as u64).wrapping_mul(FNV_PRIME)
}

pub const fn fnv_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    let mut index = 0;

    while index < bytes.len() {
        hash = fnv_byte(hash, bytes[index]);
        index += 1;
    }

    hash
}

/// Eight bytes, little endian - a fixed width rather than the shortest one, so
/// that a number is always the same number of bytes of input and can not be
/// mistaken for the start of whatever follows it.
pub const fn fnv_u64(mut hash: u64, value: u64) -> u64 {
    let bytes = value.to_le_bytes();
    let mut index = 0;

    while index < bytes.len() {
        hash = fnv_byte(hash, bytes[index]);
        index += 1;
    }

    hash
}

/// FNV-1a over a schema's canonical bytes.
///
/// **Nothing on the serving path calls this, and that is the point.** An id is
/// the constant the macro folded out of the type; the server never recomputes
/// one, never derives one from bytes, and keys its resolved-schema cache by the
/// id the client declared. The one check it makes is the stored bytes against
/// the arriving bytes, under that same declared id.
///
/// What this is for is proving the property the whole scheme rests on: canonical
/// bytes determine one number, so the same declared entity folds to the same id
/// in every process without a registry of constants agreeing about it. The tests
/// of [`super::Schema`] and [`super::SchemaBuilder`] use it to say exactly that -
/// two spellings that hash alike are one schema, and two shapes that hash apart
/// are two.
pub fn get_schema_id(schema: &[u8]) -> u64 {
    fnv_bytes(FNV_OFFSET_BASIS, schema)
}

/// Opens the hash of one declared message - the entity itself or anything a
/// field of it carries; both are messages and neither gets a rule of its own.
///
/// The caller then folds in one call per field, in the order the fields are
/// declared. The macro sorts nothing: it emits the fields in the order it read
/// them, which is the order the attribute wrote them down in, and two builds of
/// one type read one source file.
pub const fn schema_hash_begin(message_name: &str) -> u64 {
    fnv_name(FNV_OFFSET_BASIS, message_name)
}

/// A field carrying one of the nine scalars, or a run of them.
pub const fn schema_hash_scalar_field(
    hash: u64,
    no: u32,
    name: &str,
    scalar: Scalar,
    is_array: bool,
) -> u64 {
    let hash = fnv_field_head(hash, no, name, is_array);
    // What kind of thing the number after this is. Without it a scalar whose
    // code is 7 and a message whose id came out 7 would fold identically, and
    // the whole point of a rule is that it does not depend on luck.
    let hash = fnv_byte(hash, 0);

    fnv_u64(hash, scalar as u64)
}

/// A field carrying a message, named by **that message's own `SCHEMA_ID`**
/// rather than by its shape.
///
/// This is what makes the id a Merkle hash: a change anywhere inside a carried
/// message changes its constant, which changes the constant of everything that
/// carries it, all the way up to the entity - so a row written before the change
/// keeps an id nothing produces any more, which is exactly what "every row
/// remembers its own schema" needs.
///
/// A constant can not reference itself, so a message which carries itself, or
/// two which carry each other, do not compile. That is deliberate and it is the
/// project owner's decision: a row of a NoSQL table is a flat record, and a tree
/// is modelled with a second table or with `Vec<u8>`.
pub const fn schema_hash_object_field(
    hash: u64,
    no: u32,
    name: &str,
    message_id: u64,
    is_array: bool,
) -> u64 {
    let hash = fnv_field_head(hash, no, name, is_array);
    let hash = fnv_byte(hash, 1);

    fnv_u64(hash, message_id)
}

/// Everything about a field except what it carries.
const fn fnv_field_head(hash: u64, no: u32, name: &str, is_array: bool) -> u64 {
    let hash = fnv_u64(hash, no as u64);
    let hash = fnv_name(hash, name);

    fnv_byte(hash, is_array as u8)
}

/// A name, length first.
///
/// Without the length, a message `A` whose field is called `BC` and a message
/// `AB` whose field is called `C` are the same run of bytes, and two shapes
/// which fold to one id are two entities whose rows are shown through each
/// other's schema.
const fn fnv_name(hash: u64, name: &str) -> u64 {
    let bytes = name.as_bytes();

    fnv_bytes(fnv_u64(hash, bytes.len() as u64), bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The value ends up inside stored rows, so a build which computed it
    /// differently would orphan everything the previous one wrote. Pinned
    /// against the published FNV-1a vectors rather than against whatever this
    /// implementation happens to produce today.
    #[test]
    fn the_arithmetic_is_pinned_to_the_published_vectors() {
        assert_eq!(fnv_bytes(FNV_OFFSET_BASIS, b""), FNV_OFFSET_BASIS);
        assert_eq!(fnv_bytes(FNV_OFFSET_BASIS, b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(
            fnv_bytes(FNV_OFFSET_BASIS, b"foobar"),
            0x8594_4171_f739_67e8
        );

        assert_eq!(get_schema_id(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    /// Folding a slice in two goes on being folding the slice: the server hands
    /// this whole blobs, the macro hands it a name at a time.
    #[test]
    fn folding_in_pieces_is_folding_the_whole() {
        assert_eq!(
            fnv_bytes(fnv_bytes(FNV_OFFSET_BASIS, b"foo"), b"bar"),
            get_schema_id(b"foobar")
        );
    }

    /// The reason all of this is `const fn`: the macro has to be able to write
    /// the answer down as a constant, and a constant is what a `match` arm and a
    /// `where` clause can be written against later.
    #[test]
    fn a_schema_id_is_a_compile_time_constant() {
        const LIMITS: u64 = {
            let hash = schema_hash_begin("Limits");
            let hash = schema_hash_scalar_field(hash, 1, "MaxLots", Scalar::F64, false);

            schema_hash_scalar_field(hash, 2, "Allowed", Scalar::String, true)
        };

        const TRADER: u64 = {
            let hash = schema_hash_begin("TraderEntity");
            let hash = schema_hash_scalar_field(hash, 1, "PartitionKey", Scalar::String, false);

            schema_hash_object_field(hash, 5, "Limits", LIMITS, false)
        };

        assert_ne!(LIMITS, TRADER);
        assert_ne!(LIMITS, FNV_OFFSET_BASIS);
    }

    /// The Merkle part. `Limits` changing shape has to reach the entity that
    /// carries it, or the entity would keep an id whose rows no longer render
    /// the way the id promises.
    #[test]
    fn a_change_inside_a_carried_message_reaches_whatever_carries_it() {
        let trader = |limits: u64| {
            let hash = schema_hash_begin("TraderEntity");
            schema_hash_object_field(hash, 5, "Limits", limits, false)
        };

        let limits = |scalar| {
            let hash = schema_hash_begin("Limits");
            schema_hash_scalar_field(hash, 1, "MaxLots", scalar, false)
        };

        assert_ne!(limits(Scalar::F64), limits(Scalar::F32));
        assert_ne!(trader(limits(Scalar::F64)), trader(limits(Scalar::F32)));

        // And nothing else moved: the same shape is the same id, which is what
        // lets two processes agree without a registry.
        assert_eq!(trader(limits(Scalar::F64)), trader(limits(Scalar::F64)));
    }

    /// Every part of a declaration is part of the id, because every part of it
    /// changes what a stored row is shown as.
    #[test]
    fn two_shapes_which_differ_anywhere_differ_here() {
        let base = schema_hash_begin("M");

        let ids = [
            schema_hash_scalar_field(base, 1, "F", Scalar::F64, false),
            // ...the number it sits under on the wire,
            schema_hash_scalar_field(base, 2, "F", Scalar::F64, false),
            // ...the name it is shown under,
            schema_hash_scalar_field(base, 1, "G", Scalar::F64, false),
            // ...what it carries,
            schema_hash_scalar_field(base, 1, "F", Scalar::F32, false),
            // ...whether there is one of them or many,
            schema_hash_scalar_field(base, 1, "F", Scalar::F64, true),
            // ...and the message it was declared in.
            schema_hash_scalar_field(schema_hash_begin("N"), 1, "F", Scalar::F64, false),
            // A scalar and a message are told apart even when the message's id
            // is the scalar's code.
            schema_hash_object_field(base, 1, "F", Scalar::F64 as u64, false),
        ];

        for (at, left) in ids.iter().enumerate() {
            for right in ids.iter().skip(at + 1) {
                assert_ne!(left, right);
            }
        }
    }

    /// The length in front of every name, doing the job it is there for.
    #[test]
    fn a_name_can_not_borrow_a_letter_from_the_next_one() {
        let left = schema_hash_scalar_field(schema_hash_begin("A"), 1, "BC", Scalar::Bool, false);
        let right = schema_hash_scalar_field(schema_hash_begin("AB"), 1, "C", Scalar::Bool, false);

        assert_ne!(left, right);
    }
}
