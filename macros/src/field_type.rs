use proc_macro2::TokenStream;
use quote::quote;
use syn::{GenericArgument, PathArguments, Type};

/// A Rust type as protobuf sees it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ScalarType {
    Double,
    Float,
    Int64,
    Uint64,
    Int32,
    Uint32,
    Bool,
    String,
    Bytes,
}

impl ScalarType {
    /// The name of the `Scalar` variant which describes it, as the schema format
    /// in the core spells it.
    ///
    /// The two lists are the same nine and that is not a coincidence: this one is
    /// everything a Rust type can mean here, and the macro is the only thing that
    /// produces a schema, so the format has exactly what can reach it and nothing
    /// else.
    pub fn scalar_variant(&self) -> TokenStream {
        match self {
            ScalarType::Double => quote!(F64),
            ScalarType::Float => quote!(F32),
            ScalarType::Int64 => quote!(I64),
            ScalarType::Uint64 => quote!(U64),
            ScalarType::Int32 => quote!(I32),
            ScalarType::Uint32 => quote!(U32),
            ScalarType::Bool => quote!(Bool),
            ScalarType::String => quote!(String),
            ScalarType::Bytes => quote!(Bytes),
        }
    }
}

/// What a field carries: one of protobuf's own types, or a message declared with
/// `#[my_no_sql_message]`.
#[derive(Clone)]
pub enum FieldKind {
    Scalar(ScalarType),
    /// The Rust type as it was written, so the generated code can name it.
    ///
    /// Boxed because a `syn::Type` is two hundred bytes against a scalar's one,
    /// and this enum is held by value in every field of every entity the macro
    /// expands over.
    Message(Box<Type>),
}

/// How the field appears in the struct, which decides how many values it has and
/// whether it can be absent.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Cardinality {
    Single,
    Optional,
    Repeated,
}

pub struct ProtoField {
    pub kind: FieldKind,
    pub cardinality: Cardinality,
}

impl ProtoField {
    pub fn scalar(scalar: ScalarType, cardinality: Cardinality) -> Self {
        Self {
            kind: FieldKind::Scalar(scalar),
            cardinality,
        }
    }
}

/// Works out what a declared Rust type means on the wire.
///
/// `Vec<u8>` is `bytes` rather than a repeated integer, which is the mapping
/// every protobuf generator makes and the only one worth having: a repeated
/// small number is spelled `Vec<u32>`.
pub fn resolve(field_type: &Type) -> Result<ProtoField, String> {
    if let Some(inner) = generic_argument_of(field_type, "Option") {
        return Ok(ProtoField {
            kind: resolve_kind(inner)?,
            cardinality: Cardinality::Optional,
        });
    }

    if let Some(inner) = generic_argument_of(field_type, "Vec") {
        // `Vec<u8>` is one value, not many: the outer `Vec` is the bytes
        // themselves, so this is `bytes` and not a repeated anything.
        if is_byte_vector(field_type) {
            return Ok(ProtoField::scalar(ScalarType::Bytes, Cardinality::Single));
        }

        return Ok(ProtoField {
            kind: resolve_kind(inner)?,
            cardinality: Cardinality::Repeated,
        });
    }

    Ok(ProtoField {
        kind: resolve_kind(field_type)?,
        cardinality: Cardinality::Single,
    })
}

/// Anything which is not one of protobuf's own types is taken to be a message.
///
/// That is the only rule which can work: a macro sees one struct at a time and
/// can not look at the type a field names. What keeps a typo from becoming a
/// mysterious trait error is the list below - the primitives protobuf has no
/// type for are the ones people actually reach for by mistake, and they are
/// refused by name.
fn resolve_kind(field_type: &Type) -> Result<FieldKind, String> {
    // Asked before anything else, because `Vec<u8>` means `bytes` wherever it
    // stands - `Option<Vec<u8>>` is an optional byte string and `Vec<Vec<u8>>`
    // a repeated one, exactly as they read. Left to the fallback below it is a
    // type with generic arguments, which is to say refused.
    if is_byte_vector(field_type) {
        return Ok(FieldKind::Scalar(ScalarType::Bytes));
    }

    let Some(name) = type_name(field_type) else {
        return Err(unsupported(field_type));
    };

    Ok(match name.as_str() {
        "f64" => FieldKind::Scalar(ScalarType::Double),
        "f32" => FieldKind::Scalar(ScalarType::Float),
        "i64" => FieldKind::Scalar(ScalarType::Int64),
        "u64" => FieldKind::Scalar(ScalarType::Uint64),
        "i32" => FieldKind::Scalar(ScalarType::Int32),
        "u32" => FieldKind::Scalar(ScalarType::Uint32),
        "bool" => FieldKind::Scalar(ScalarType::Bool),
        "String" => FieldKind::Scalar(ScalarType::String),

        // Protobuf has no type for these. Left to the fallback they would be
        // read as messages and fail with a trait bound nobody would connect to
        // the real mistake.
        "u8" | "u16" | "i8" | "i16" | "i128" | "u128" | "usize" | "isize" | "char" | "str" => {
            return Err(unsupported(field_type));
        }

        // Neither can a type with generic arguments of its own be a message:
        // both macros declare a plain struct, so `HashMap<..>`, `Box<..>` and
        // the rest are mistakes rather than messages, and saying so here beats
        // a trait bound further down.
        _ if has_generic_arguments(field_type) => return Err(unsupported(field_type)),

        _ => FieldKind::Message(Box::new(field_type.clone())),
    })
}

fn unsupported(field_type: &Type) -> String {
    format!(
        "'{}' has no protobuf type. A field may be String, bool, i32, i64, u32, u64, f32, f64, Vec<u8>, or a struct declared with #[my_no_sql_message] - and any of those inside Option<> or Vec<>. A field which is not meant to be stored simply has no #[proto_no(..)]",
        quote!(#field_type)
    )
}

fn is_byte_vector(field_type: &Type) -> bool {
    let Some(inner) = generic_argument_of(field_type, "Vec") else {
        return false;
    };

    type_name(inner).as_deref() == Some("u8")
}

fn has_generic_arguments(field_type: &Type) -> bool {
    let Type::Path(path) = field_type else {
        return false;
    };

    let Some(segment) = path.path.segments.last() else {
        return false;
    };

    !matches!(segment.arguments, PathArguments::None)
}

/// The last segment of a path type - `Vec<u8>` is `Vec`, `std::string::String`
/// is `String`.
pub fn type_name(field_type: &Type) -> Option<String> {
    let Type::Path(path) = field_type else {
        return None;
    };

    Some(path.path.segments.last()?.ident.to_string())
}

fn generic_argument_of<'s>(field_type: &'s Type, wrapper: &str) -> Option<&'s Type> {
    let Type::Path(path) = field_type else {
        return None;
    };

    let segment = path.path.segments.last()?;

    if segment.ident != wrapper {
        return None;
    }

    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return None;
    };

    match arguments.args.first()? {
        GenericArgument::Type(inner) => Some(inner),
        _ => None,
    }
}
