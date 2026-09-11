use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Attribute, Error, Fields, ItemStruct, LitStr, Result, Token};

use crate::codegen::{self, EntityField};
use crate::field_type::{Cardinality, ProtoField, ScalarType};
use crate::fields::{derives_default, read_fields};

/// The four field numbers the contract reserves, and the names they are known
/// by wherever this server is spoken to.
const RESERVED: [(&str, &str); 4] = [
    ("partition_key", "PartitionKey"),
    ("row_key", "RowKey"),
    ("time_stamp", "TimeStamp"),
    ("expires", "Expires"),
];

/// The first number an entity may use for a field of its own.
const FIRST_USER_FIELD: u32 = 5;

/// `TimeStamp` and `Expires`, which are written and read like any other field
/// but are **not** described.
///
/// They are the server's own values: they are not in the stored row at all, and
/// the server puts them back on the way out - so a schema naming them would be
/// describing something that is never there. `PartitionKey` and `RowKey` are
/// ordinary fields of the row and are described as such, which is what keeps the
/// renderer from having to know that any number is reserved.
const NOT_IN_THE_SCHEMA: [u32; 2] = [3, 4];

pub fn generate(attr: TokenStream, item: TokenStream) -> Result<TokenStream> {
    let arguments: Arguments = syn::parse2(attr)?;
    let source: ItemStruct = syn::parse2(item)?;

    let fields = read_fields(&source, "an entity", FIRST_USER_FIELD, &RESERVED)?;

    let struct_name = &source.ident;
    let table_name = arguments.table_name;

    let rewritten = rewrite_struct(&source)?;

    // Everything the entity puts on the wire: the four reserved fields, then
    // whatever was declared.
    let on_the_wire: Vec<EntityField> = reserved_fields().into_iter().chain(fields).collect();

    let write: Vec<TokenStream> = on_the_wire.iter().map(codegen::write_field).collect();
    let read: Vec<TokenStream> = on_the_wire.iter().map(codegen::read_field).collect();

    let described: Vec<&EntityField> = on_the_wire
        .iter()
        .filter(|field| !NOT_IN_THE_SCHEMA.contains(&field.number))
        .collect();

    let describe: Vec<TokenStream> = described
        .iter()
        .copied()
        .map(codegen::describe_field)
        .collect();
    let hash: Vec<TokenStream> = described.iter().copied().map(codegen::hash_field).collect();

    let declare = codegen::declare_messages(&on_the_wire);

    let message_name = struct_name.to_string();

    Ok(quote! {
        #rewritten

        impl #struct_name {
            pub const TABLE_NAME: &'static str = #table_name;
        }

        impl my_no_sql_grpc_core::MyNoSqlEntity for #struct_name {
            const TABLE_NAME: &'static str = #table_name;

            const SCHEMA_ID: u64 = {
                let hash = my_no_sql_grpc_core::schemas::schema_hash_begin(#message_name);
                #(#hash)*
                hash
            };

            fn get_schema() -> &'static my_no_sql_grpc_core::MyNoSqlEntitySchema {
                static SCHEMA: std::sync::OnceLock<my_no_sql_grpc_core::MyNoSqlEntitySchema> =
                    std::sync::OnceLock::new();

                SCHEMA.get_or_init(|| {
                    let builder =
                        my_no_sql_grpc_core::schemas::SchemaBuilder::new(#message_name)
                        #(.add_field(#describe))*;

                    // Every message the entity carries is declared beside it, so
                    // a nested value can be shown under its own field names
                    // instead of as a blob.
                    #(#declare)*

                    my_no_sql_grpc_core::MyNoSqlEntitySchema::new(
                        <Self as my_no_sql_grpc_core::MyNoSqlEntity>::SCHEMA_ID,
                        builder.build().serialize(),
                    )
                })
            }

            fn get_partition_key(&self) -> &str {
                &self.partition_key
            }

            fn get_row_key(&self) -> &str {
                &self.row_key
            }

            fn serialize(&self, dest: &mut Vec<u8>) {
                #(#write)*
            }

            fn from_slice(
                src: &[u8],
            ) -> std::result::Result<Self, my_no_sql_grpc_core::db_entity::DbEntityParseFail> {
                let mut result = Self::default();
                let mut reader = my_no_sql_grpc_core::db_entity::ProtobufReader::new(src);

                while let Some(field) = reader.get_next()? {
                    match field.field_no {
                        #(#read)*
                        // A number this build does not know about was written by
                        // one that did. Skipping it is what keeps such a row
                        // readable instead of refused.
                        _ => {}
                    }
                }

                Ok(result)
            }
        }
    })
}

/// The four reserved fields, described exactly as the contract spells them.
fn reserved_fields() -> Vec<EntityField> {
    RESERVED
        .iter()
        .enumerate()
        .map(|(index, (ident, proto_name))| EntityField {
            ident: syn::Ident::new(ident, proc_macro2::Span::call_site()),
            proto_name: proto_name.to_string(),
            number: index as u32 + 1,
            proto: ProtoField::scalar(
                if index < 2 {
                    ScalarType::String
                } else {
                    ScalarType::Int64
                },
                Cardinality::Single,
            ),
        })
        .collect()
}

/// Emits the struct with the reserved fields in front and `proto_no` taken back
/// off - it has done its job by now, and leaving it would be an attribute
/// nothing knows.
fn rewrite_struct(source: &ItemStruct) -> Result<TokenStream> {
    let Fields::Named(named) = &source.fields else {
        return Err(Error::new_spanned(
            source,
            "an entity is a struct with named fields",
        ));
    };

    let attrs = &source.attrs;
    let vis = &source.vis;
    let ident = &source.ident;

    let declared: Vec<TokenStream> = named
        .named
        .iter()
        .map(|field| {
            let attrs: Vec<&Attribute> = field
                .attrs
                .iter()
                .filter(|attr| !attr.path().is_ident("proto_no"))
                .collect();

            let vis = &field.vis;
            let name = &field.ident;
            let ty = &field.ty;

            quote! { #(#attrs)* #vis #name: #ty }
        })
        .collect();

    // `from_slice` starts from the default and fills in what the row carried,
    // which is what proto3 means by an absent field. Derived here rather than
    // asked of the caller - unless the caller already did it.
    let default = if derives_default(attrs) {
        quote!()
    } else {
        quote!(#[derive(Default)])
    };

    Ok(quote! {
        #(#attrs)*
        #default
        #vis struct #ident {
            /// `string PartitionKey = 1;`
            pub partition_key: String,
            /// `string RowKey = 2;`
            pub row_key: String,
            /// `int64 TimeStamp = 3;` - unix microseconds. Left at 0 unless the
            /// application means to keep its own; the server stamps its clock
            /// then.
            pub time_stamp: i64,
            /// `int64 Expires = 4;` - unix microseconds, 0 means never.
            pub expires: i64,
            #(#declared,)*
        }
    })
}

/// `table_name: "traders"`
///
/// There is no package here any more, and there is nowhere for one to go: a
/// schema holds one message per name and refers to it by index, so a name never
/// has to be qualified to be resolved.
struct Arguments {
    table_name: String,
}

impl Parse for Arguments {
    fn parse(input: ParseStream) -> Result<Self> {
        let mut table_name = None;

        while !input.is_empty() {
            let key: syn::Ident = input.parse()?;
            input.parse::<Token![:]>()?;
            let value: LitStr = input.parse()?;

            match key.to_string().as_str() {
                "table_name" => table_name = Some(value.value()),
                other => {
                    return Err(Error::new_spanned(
                        key,
                        format!(
                            "'{other}' is not an argument of my_no_sql_entity - it takes table_name and nothing else"
                        ),
                    ));
                }
            }

            if input.is_empty() {
                break;
            }

            input.parse::<Token![,]>()?;
        }

        let Some(table_name) = table_name else {
            return Err(input.error(
                "my_no_sql_entity needs a table_name: #[my_no_sql_entity(table_name: \"traders\")]",
            ));
        };

        Ok(Self { table_name })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fields::to_pascal_case;

    #[test]
    fn snake_case_becomes_pascal_case() {
        assert_eq!(to_pascal_case("amount"), "Amount");
        assert_eq!(to_pascal_case("instrument_id"), "InstrumentId");
        assert_eq!(to_pascal_case("a_b_c"), "ABC");
    }

    #[test]
    fn a_reserved_field_number_is_refused() {
        let err = generate(
            quote!(table_name: "traders"),
            quote! {
                pub struct TraderEntity {
                    #[proto_no(3)]
                    pub amount: f64,
                }
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("reserved"), "{err}");
    }

    #[test]
    fn two_fields_can_not_share_a_number() {
        let err = generate(
            quote!(table_name: "traders"),
            quote! {
                pub struct TraderEntity {
                    #[proto_no(5)]
                    pub amount: f64,
                    #[proto_no(5)]
                    pub other: f64,
                }
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("another field"), "{err}");
    }

    #[test]
    fn a_field_which_shadows_a_reserved_one_is_refused() {
        let err = generate(
            quote!(table_name: "traders"),
            quote! {
                pub struct TraderEntity {
                    pub row_key: String,
                }
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("reserves"), "{err}");
    }

    #[test]
    fn a_type_with_no_protobuf_meaning_is_refused() {
        let err = generate(
            quote!(table_name: "traders"),
            quote! {
                pub struct TraderEntity {
                    #[proto_no(5)]
                    pub amount: std::collections::HashMap<String, String>,
                }
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("no protobuf type"), "{err}");
    }

    /// A field nobody marked is left alone: it stays in the struct and never
    /// touches the wire.
    #[test]
    fn an_unmarked_field_is_not_serialized() {
        let generated = generate(
            quote!(table_name: "traders"),
            quote! {
                pub struct TraderEntity {
                    #[proto_no(5)]
                    pub amount: f64,
                    pub computed_locally: u64,
                }
            },
        )
        .unwrap()
        .to_string();

        assert!(generated.contains("computed_locally"));
        // The only numbers written are the four reserved ones and 5.
        assert!(generated.contains("5u32"));
        assert!(!generated.contains("6u32"));
    }

    /// The macro rewrites the struct into a plain one, so anything generic is
    /// dropped on the way through and the caller is left with a trait error
    /// about a struct they never wrote. Said out loud instead.
    #[test]
    fn a_generic_entity_is_refused() {
        for item in [
            quote! {
                pub struct TraderEntity<T> {
                    #[proto_no(5)]
                    pub amount: T,
                }
            },
            quote! {
                pub struct TraderEntity<'s> {
                    #[proto_no(5)]
                    pub name: &'s str,
                }
            },
            quote! {
                pub struct TraderEntity<T> where T: Clone {
                    #[proto_no(5)]
                    pub amount: f64,
                }
            },
        ] {
            let err = generate(quote!(table_name: "traders"), item).unwrap_err();

            assert!(err.to_string().contains("can not be generic"), "{err}");
        }
    }

    /// The same refusal an entity gets, and it matters here too: an entity is a
    /// message like any other as far as the id is concerned.
    #[test]
    fn an_entity_which_carries_itself_is_refused() {
        let err = generate(
            quote!(table_name: "traders"),
            quote! {
                pub struct TraderEntity {
                    #[proto_no(5)]
                    pub parent: Option<TraderEntity>,
                }
            },
        )
        .unwrap_err();

        assert!(
            err.to_string().contains("'TraderEntity' carries itself"),
            "{err}"
        );
    }

    /// The schema describes `PartitionKey` and `RowKey` as ordinary fields, so
    /// the renderer never has to know that a number is reserved. `TimeStamp` and
    /// `Expires` are not in it at all - they are not in the stored row either.
    #[test]
    fn the_two_moments_are_written_but_not_described() {
        let generated = generate(
            quote!(table_name: "traders"),
            quote! {
                pub struct TraderEntity {
                    #[proto_no(5)]
                    pub amount: f64,
                }
            },
        )
        .unwrap()
        .to_string();

        for described in ["PartitionKey", "RowKey", "Amount"] {
            assert!(
                generated.contains(&format!("DeclaredField :: scalar ({described:?}")),
                "'{described}' is not in the schema"
            );
        }

        for owned_by_the_server in ["TimeStamp", "Expires"] {
            assert!(
                !generated.contains(&format!("DeclaredField :: scalar ({owned_by_the_server:?}")),
                "'{owned_by_the_server}' has no business in the schema"
            );
            // ...and it is still written and read, which is what makes it
            // "described by nobody" rather than "gone".
            assert!(generated.contains("self . time_stamp"));
            assert!(generated.contains("self . expires"));
        }
    }

    #[test]
    fn the_table_name_is_required() {
        assert!(
            generate(
                quote!(),
                quote!(
                    pub struct A {}
                )
            )
            .is_err()
        );
    }
}
