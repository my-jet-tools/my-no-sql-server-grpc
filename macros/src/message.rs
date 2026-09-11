use proc_macro2::TokenStream;
use quote::quote;
use syn::{Attribute, Error, Fields, ItemStruct, Result};

use crate::codegen;
use crate::fields::{derives_default, read_fields};

/// A message may use every field number protobuf allows. The four the entity
/// contract reserves are the entity's own - a message carries no keys, no
/// timestamp and no expiry, so nothing here has to be kept out of the way.
const FIRST_FIELD: u32 = 1;

pub fn generate(item: TokenStream) -> Result<TokenStream> {
    let source: ItemStruct = syn::parse2(item)?;

    let fields = read_fields(&source, "a message", FIRST_FIELD, &[])?;

    let struct_name = &source.ident;
    let message_name = struct_name.to_string();

    let rewritten = rewrite_struct(&source)?;

    let describe: Vec<TokenStream> = fields.iter().map(codegen::describe_field).collect();
    let hash: Vec<TokenStream> = fields.iter().map(codegen::hash_field).collect();
    let declare = codegen::declare_messages(&fields);
    let write: Vec<TokenStream> = fields.iter().map(codegen::write_field).collect();
    let read: Vec<TokenStream> = fields.iter().map(codegen::read_field).collect();

    Ok(quote! {
        #rewritten

        impl my_no_sql_grpc_core::MyNoSqlMessage for #struct_name {
            const MESSAGE_NAME: &'static str = #message_name;

            const SCHEMA_ID: u64 = {
                let hash = my_no_sql_grpc_core::schemas::schema_hash_begin(#message_name);
                #(#hash)*
                hash
            };

            fn declare(
                builder: my_no_sql_grpc_core::schemas::SchemaBuilder,
            ) -> my_no_sql_grpc_core::schemas::SchemaBuilder {
                // Asked before it is told: the same message reached through two
                // fields is one declaration.
                if builder.has_message(#message_name) {
                    return builder;
                }

                let builder = builder.add_message(
                    #message_name,
                    vec![#(#describe),*],
                );

                #(#declare)*

                builder
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
                        // one that did.
                        _ => {}
                    }
                }

                Ok(result)
            }
        }
    })
}

/// Unlike an entity, nothing is put into a message: it is exactly what was
/// declared. `proto_no` is taken back off - it has done its job, and leaving it
/// would be an attribute nothing knows.
fn rewrite_struct(source: &ItemStruct) -> Result<TokenStream> {
    let Fields::Named(named) = &source.fields else {
        return Err(Error::new_spanned(
            source,
            "a message is a struct with named fields",
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

    // `from_slice` starts from the default and fills in what the payload
    // carried, which is what proto3 means by an absent field.
    let default = if derives_default(attrs) {
        quote!()
    } else {
        quote!(#[derive(Default)])
    };

    Ok(quote! {
        #(#attrs)*
        #default
        #vis struct #ident {
            #(#declared,)*
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same refusal as for an entity, and for the same reason: what is
    /// emitted is a plain struct, so `Wrapper<T>` would expand into a trait
    /// error about a declaration nobody wrote.
    #[test]
    fn a_generic_message_is_refused() {
        let err = generate(quote! {
            pub struct Wrapper<T> {
                #[proto_no(1)]
                pub value: T,
            }
        })
        .unwrap_err();

        assert!(err.to_string().contains("can not be generic"), "{err}");
    }

    /// Said here rather than left to rustc. The constant a message's id is can
    /// not name itself, and what that produces on its own is `cycle detected
    /// when evaluating`, pointing at generated code nobody wrote.
    #[test]
    fn a_message_which_carries_itself_is_refused() {
        for item in [
            quote! {
                pub struct Node {
                    #[proto_no(1)]
                    pub next: Node,
                }
            },
            quote! {
                pub struct Node {
                    #[proto_no(1)]
                    pub children: Vec<Node>,
                }
            },
            quote! {
                pub struct Node {
                    #[proto_no(1)]
                    pub parent: Option<Node>,
                }
            },
        ] {
            let err = generate(item).unwrap_err();

            assert!(err.to_string().contains("'Node' carries itself"), "{err}");
            assert!(err.to_string().contains("flat record"), "{err}");
        }
    }
}
