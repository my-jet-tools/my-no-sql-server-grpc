use proc_macro2::TokenStream;
use quote::quote;
use syn::Ident;

use crate::field_type::{Cardinality, FieldKind, ProtoField, ScalarType};

/// One field of the entity as the generator sees it: where it lives in the
/// struct, and what it is on the wire.
pub struct EntityField {
    pub ident: Ident,
    pub proto_name: String,
    pub number: u32,
    pub proto: ProtoField,
}

/// Appends the field. Proto3 leaves a default value off the wire and so does
/// this - which is also how `TimeStamp` stays absent until somebody sets it,
/// letting the server stamp its own clock.
///
/// A field declared `Option<..>` is the exception: it is written whenever it is
/// `Some`, default value or not, because that is the whole difference between
/// "zero" and "not set".
pub fn write_field(field: &EntityField) -> TokenStream {
    let ident = &field.ident;
    let number = field.number;

    let scalar = match &field.proto.kind {
        FieldKind::Scalar(scalar) => *scalar,
        // A message has explicit presence in protobuf, so there is no default
        // to leave out: a field of this type in the struct is a value somebody
        // put there. `Option<..>` is how "not set" is spelled.
        FieldKind::Message(_) => return write_message_field(field),
    };

    match field.proto.cardinality {
        Cardinality::Single => {
            let write = write_value(scalar, number, quote!(self.#ident));
            let is_default = is_default(scalar, quote!(self.#ident));

            quote! {
                if !(#is_default) {
                    #write
                }
            }
        }

        Cardinality::Optional => {
            let write = write_value(scalar, number, quote!((*value)));

            quote! {
                if let Some(value) = self.#ident.as_ref() {
                    #write
                }
            }
        }

        Cardinality::Repeated => {
            let write = write_value(scalar, number, quote!((*value)));

            quote! {
                for value in self.#ident.iter() {
                    #write
                }
            }
        }
    }
}

/// A nested message is written the way protobuf writes one: built into its own
/// buffer, then put on the wire as a length-delimited field. The length has to
/// be known before the tag, and it is only known once the message is built.
fn write_message_field(field: &EntityField) -> TokenStream {
    let ident = &field.ident;
    let number = field.number;

    let write = quote! {
        let mut nested = Vec::new();
        my_no_sql_grpc_core::MyNoSqlMessage::serialize(value, &mut nested);
        my_no_sql_grpc_core::db_entity::write_len_field(dest, #number, &nested);
    };

    match field.proto.cardinality {
        Cardinality::Single => quote! {
            {
                let value = &self.#ident;
                #write
            }
        },

        Cardinality::Optional => quote! {
            if let Some(value) = self.#ident.as_ref() {
                #write
            }
        },

        // Repeated messages are never packed - packing is for the fixed-width
        // and varint types only.
        Cardinality::Repeated => quote! {
            for value in self.#ident.iter() {
                #write
            }
        },
    }
}

/// Whether the value is what proto3 calls the default, and therefore left out.
fn is_default(scalar: ScalarType, value: TokenStream) -> TokenStream {
    match scalar {
        ScalarType::String | ScalarType::Bytes => quote!(#value.is_empty()),
        ScalarType::Bool => quote!(!#value),
        ScalarType::Double | ScalarType::Float => quote!(#value == 0.0),
        _ => quote!(#value == 0),
    }
}

fn write_value(scalar: ScalarType, number: u32, value: TokenStream) -> TokenStream {
    match scalar {
        ScalarType::String => quote! {
            my_no_sql_grpc_core::db_entity::write_len_field(dest, #number, #value.as_bytes());
        },
        ScalarType::Bytes => quote! {
            my_no_sql_grpc_core::db_entity::write_len_field(dest, #number, &#value);
        },
        ScalarType::Bool => quote! {
            my_no_sql_grpc_core::db_entity::write_varint_field(
                dest, #number, if #value { 1 } else { 0 },
            );
        },
        ScalarType::Double => quote! {
            my_no_sql_grpc_core::db_entity::write_i64_field(dest, #number, #value.to_bits());
        },
        ScalarType::Float => quote! {
            my_no_sql_grpc_core::db_entity::write_i32_field(dest, #number, #value.to_bits());
        },
        // Every integer goes as a varint, and the cast sign-extends a negative
        // one through 64 bits - which is exactly what protobuf does to an
        // `int32`/`int64`.
        _ => quote! {
            my_no_sql_grpc_core::db_entity::write_varint_field(dest, #number, #value as u64);
        },
    }
}

/// The arm which takes one occurrence of the field off the wire.
pub fn read_field(field: &EntityField) -> TokenStream {
    let ident = &field.ident;
    let number = field.number;

    let scalar = match &field.proto.kind {
        FieldKind::Scalar(scalar) => *scalar,
        FieldKind::Message(message_type) => {
            let read = quote! {
                <#message_type as my_no_sql_grpc_core::MyNoSqlMessage>::from_slice(
                    field.read_message_slice(src)?,
                )?
            };

            return match field.proto.cardinality {
                Cardinality::Single => quote! { #number => { result.#ident = #read; } },
                Cardinality::Optional => quote! { #number => { result.#ident = Some(#read); } },
                Cardinality::Repeated => quote! { #number => { result.#ident.push(#read); } },
            };
        }
    };

    match field.proto.cardinality {
        Cardinality::Single => {
            let read = read_value(scalar);
            quote! { #number => { result.#ident = #read; } }
        }

        Cardinality::Optional => {
            let read = read_value(scalar);
            quote! { #number => { result.#ident = Some(#read); } }
        }

        // A repeated numeric may arrive packed - protoc emits it that way by
        // default - so both shapes have to be understood, whoever wrote the row.
        Cardinality::Repeated => match packed(scalar) {
            Some(packed) => {
                let read_packed = packed.read;
                let from_u64 = packed.from_u64;
                let read = read_value(scalar);

                quote! {
                    #number => {
                        if field.wire_type == my_no_sql_grpc_core::db_entity::consts::WIRE_TYPE_LEN {
                            for value in #read_packed(field.value.get_slice(src))? {
                                result.#ident.push(#from_u64);
                            }
                        } else {
                            result.#ident.push(#read);
                        }
                    }
                }
            }
            None => {
                let read = read_value(scalar);
                quote! { #number => { result.#ident.push(#read); } }
            }
        },
    }
}

struct Packed {
    /// The core helper which takes the run apart into `u64`s.
    read: TokenStream,
    /// How one of those becomes a value of the field's own type. Written over an
    /// identifier called `value`.
    from_u64: TokenStream,
}

/// How a packed run of this type is read, or `None` for the types which can
/// never be packed - a string and a byte string carry their own length.
fn packed(scalar: ScalarType) -> Option<Packed> {
    let (read, from_u64) = match scalar {
        ScalarType::String | ScalarType::Bytes => return None,
        ScalarType::Double => (
            quote!(my_no_sql_grpc_core::db_entity::read_packed_i64),
            quote!(f64::from_bits(value)),
        ),
        ScalarType::Float => (
            quote!(my_no_sql_grpc_core::db_entity::read_packed_i32),
            quote!(f32::from_bits(value as u32)),
        ),
        ScalarType::Bool => (
            quote!(my_no_sql_grpc_core::db_entity::read_packed_varints),
            quote!(value != 0),
        ),
        ScalarType::Int32 => (
            quote!(my_no_sql_grpc_core::db_entity::read_packed_varints),
            quote!(value as i32),
        ),
        ScalarType::Int64 => (
            quote!(my_no_sql_grpc_core::db_entity::read_packed_varints),
            quote!(value as i64),
        ),
        ScalarType::Uint32 => (
            quote!(my_no_sql_grpc_core::db_entity::read_packed_varints),
            quote!(value as u32),
        ),
        ScalarType::Uint64 => (
            quote!(my_no_sql_grpc_core::db_entity::read_packed_varints),
            quote!(value),
        ),
    };

    Some(Packed { read, from_u64 })
}

fn read_value(scalar: ScalarType) -> TokenStream {
    match scalar {
        ScalarType::String => quote!(field.read_string(src)?),
        ScalarType::Bytes => quote!(field.read_bytes(src)?),
        ScalarType::Double => quote!(f64::from_bits(field.read_i64()?)),
        ScalarType::Float => quote!(f32::from_bits(field.read_i32()?)),
        ScalarType::Bool => quote!(field.read_varint()? != 0),
        ScalarType::Int32 => quote!(field.read_varint()? as i32),
        ScalarType::Int64 => quote!(field.read_varint()? as i64),
        ScalarType::Uint32 => quote!(field.read_varint()? as u32),
        ScalarType::Uint64 => quote!(field.read_varint()?),
    }
}

/// One line of the schema: this is what makes a stored row renderable under its
/// own field names.
pub fn describe_field(field: &EntityField) -> TokenStream {
    let name = &field.proto_name;
    let number = field.number;
    let repeated = field.proto.cardinality == Cardinality::Repeated;

    match &field.proto.kind {
        FieldKind::Scalar(scalar) => {
            let variant = scalar.scalar_variant();

            quote! {
                my_no_sql_grpc_core::schemas::DeclaredField::scalar(
                    #name,
                    #number,
                    my_no_sql_grpc_core::schemas::Scalar::#variant,
                    #repeated,
                )
            }
        }

        FieldKind::Message(message_type) => quote! {
            my_no_sql_grpc_core::schemas::DeclaredField::object(
                #name,
                #number,
                <#message_type as my_no_sql_grpc_core::MyNoSqlMessage>::MESSAGE_NAME,
                #repeated,
            )
        },
    }
}

/// One fold of the id, over an identifier called `hash`.
///
/// The fields are emitted in the order they were read, which is the order the
/// attribute wrote them down in: the helpers in the core sort nothing, and they
/// do not have to - two builds of one type read one source file, while two
/// *processes* agree because they compile the same file.
///
/// A carried message contributes its own `SCHEMA_ID` rather than its shape, and
/// that is the whole Merkle rule: a change anywhere inside it reaches the
/// constant of everything that carries it.
pub fn hash_field(field: &EntityField) -> TokenStream {
    let name = &field.proto_name;
    let number = field.number;
    let repeated = field.proto.cardinality == Cardinality::Repeated;

    match &field.proto.kind {
        FieldKind::Scalar(scalar) => {
            let variant = scalar.scalar_variant();

            quote! {
                let hash = my_no_sql_grpc_core::schemas::schema_hash_scalar_field(
                    hash,
                    #number,
                    #name,
                    my_no_sql_grpc_core::schemas::Scalar::#variant,
                    #repeated,
                );
            }
        }

        FieldKind::Message(message_type) => quote! {
            let hash = my_no_sql_grpc_core::schemas::schema_hash_object_field(
                hash,
                #number,
                #name,
                <#message_type as my_no_sql_grpc_core::MyNoSqlMessage>::SCHEMA_ID,
                #repeated,
            );
        },
    }
}

/// Every message this struct carries, so the schema can declare them beside it.
/// A message reached through two fields is declared once - the builder is asked
/// before it is told.
pub fn declare_messages(fields: &[EntityField]) -> Vec<TokenStream> {
    fields
        .iter()
        .filter_map(|field| match &field.proto.kind {
            FieldKind::Scalar(_) => None,
            FieldKind::Message(message_type) => Some(quote! {
                let builder =
                    <#message_type as my_no_sql_grpc_core::MyNoSqlMessage>::declare(builder);
            }),
        })
        .collect()
}
