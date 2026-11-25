use std::fmt;

use check_syntax::copy::Text;

use crate::data::Data;
use serde::de::{self, IntoDeserializer, Visitor};

#[derive(Debug)]
pub struct Error {
    /// Where in the value it went wrong, like `portals[1].address`.
    pub path: String,
    pub message: String,
}

impl Error {
    fn at(mut self, step: &str) -> Error {
        self.path.insert_str(0, step);
        self
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.path.strip_prefix('.').unwrap_or(&self.path) {
            "" => write!(f, "{}", self.message),
            path => write!(f, "{path}: {}", self.message),
        }
    }
}

impl std::error::Error for Error {}

impl de::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Error {
        Error {
            path: String::new(),
            message: msg.to_string(),
        }
    }
}

pub struct ValueDe(pub Data);

impl<'de> de::Deserializer<'de> for ValueDe {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.0 {
            Data::Int(n) => visitor.visit_i64(n),
            Data::Float(n) => visitor.visit_f64(n),
            Data::Bool(b) => visitor.visit_bool(b),
            Data::Str(s) => visitor.visit_string(s),
            Data::List(items) => visitor.visit_seq(SeqDe::new(items)),
            Data::Map(entries) => visitor.visit_map(MapDe::new(entries, false)),
            Data::Some(v) => visitor.visit_some(ValueDe(*v)),
            Data::None => visitor.visit_none(),
            Data::FieldTag(mut items) if items.len() == 1 => {
                ValueDe(items.remove(0)).deserialize_any(visitor)
            }
            Data::FieldTag(items) => visitor.visit_seq(SeqDe::new(items)),
            Data::Named {
                name: Some(name),
                is_variant: true,
                field_names,
                fields,
            } => visitor.visit_enum(EnumDe {
                name,
                field_names,
                fields,
            }),
            Data::Named {
                field_names: Some(names),
                fields,
                ..
            } => visitor.visit_map(MapDe::fields(names, fields)),
            Data::Named { fields, .. } => visitor.visit_seq(SeqDe::new(fields)),
            Data::Other => Err(de::Error::custom(Text::con_value_unsupported.get())),
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.0 {
            Data::Some(v) => visitor.visit_some(ValueDe(*v)),
            Data::None => visitor.visit_none(),
            _ => visitor.visit_some(self),
        }
    }

    /// Without the checker, `Name(...)` could be a class or a variant; an enum takes it as a variant.
    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        match self.0 {
            Data::FieldTag(mut items) if items.len() == 1 => {
                ValueDe(items.remove(0)).deserialize_enum(name, variants, visitor)
            }
            Data::Named {
                name: Some(name),
                field_names,
                fields,
                ..
            } => visitor.visit_enum(EnumDe {
                name,
                field_names,
                fields,
            }),
            data => ValueDe(data).deserialize_any(visitor),
        }
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.0 {
            Data::FieldTag(items) => visitor.visit_seq(SeqDe::new(items)),
            data => ValueDe(data).deserialize_any(visitor),
        }
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_unit()
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf
        unit unit_struct tuple tuple_struct map struct identifier
    }
}

struct SeqDe {
    items: std::vec::IntoIter<Data>,
    index: usize,
}

impl SeqDe {
    fn new(items: Vec<Data>) -> SeqDe {
        SeqDe {
            items: items.into_iter(),
            index: 0,
        }
    }
}

impl<'de> de::SeqAccess<'de> for SeqDe {
    type Error = Error;

    fn next_element_seed<T: de::DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Error> {
        let Some(item) = self.items.next() else {
            return Ok(None);
        };
        let index = self.index;
        self.index += 1;
        seed.deserialize(ValueDe(item))
            .map(Some)
            .map_err(|e| e.at(&format!("[{index}]")))
    }
}

/// A map's entries, or a record's fields keyed by name.
struct MapDe {
    entries: std::vec::IntoIter<(Data, Data)>,
    is_record: bool,
    next: Option<(String, Data)>,
}

impl MapDe {
    fn new(entries: Vec<(Data, Data)>, is_record: bool) -> MapDe {
        MapDe {
            entries: entries.into_iter(),
            is_record,
            next: None,
        }
    }

    fn fields(names: Vec<String>, fields: Vec<Data>) -> MapDe {
        MapDe::new(names.into_iter().map(Data::Str).zip(fields).collect(), true)
    }
}

impl<'de> de::MapAccess<'de> for MapDe {
    type Error = Error;

    fn next_key_seed<K: de::DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        let Some((key, value)) = self.entries.next() else {
            return Ok(None);
        };
        let step = match (&key, self.is_record) {
            (Data::Str(name), true) => format!(".{name}"),
            (Data::Str(s), false) => format!("[{s:?}]"),
            (Data::Int(n), _) => format!("[{n}]"),
            _ => "[_]".to_string(),
        };
        let key = seed.deserialize(ValueDe(key)).map_err(|e| e.at(&step))?;
        self.next = Some((step, value));
        Ok(Some(key))
    }

    fn next_value_seed<V: de::DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        let (step, value) = self.next.take().unwrap();
        seed.deserialize(ValueDe(value)).map_err(|e| e.at(&step))
    }
}

struct EnumDe {
    name: String,
    field_names: Option<Vec<String>>,
    fields: Vec<Data>,
}

impl<'de> de::EnumAccess<'de> for EnumDe {
    type Error = Error;
    type Variant = EnumDe;

    fn variant_seed<V: de::DeserializeSeed<'de>>(
        self,
        seed: V,
    ) -> Result<(V::Value, EnumDe), Error> {
        let variant = seed.deserialize(self.name.as_str().into_deserializer())?;
        Ok((variant, self))
    }
}

impl<'de> de::VariantAccess<'de> for EnumDe {
    type Error = Error;

    fn unit_variant(self) -> Result<(), Error> {
        match self.fields.len() {
            0 => Ok(()),
            _ => Err(de::Error::invalid_type(
                de::Unexpected::TupleVariant,
                &"unit variant",
            )),
        }
    }

    fn newtype_variant_seed<T: de::DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value, Error> {
        match <[Data; 1]>::try_from(self.fields) {
            Ok([field]) => seed.deserialize(ValueDe(field)),
            Err(_) => Err(de::Error::invalid_type(
                de::Unexpected::TupleVariant,
                &"newtype variant",
            )),
        }
    }

    fn tuple_variant<V: Visitor<'de>>(self, _len: usize, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_seq(SeqDe::new(self.fields))
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        match self.field_names {
            Some(names) => visitor.visit_map(MapDe::fields(names, self.fields)),
            None => visitor.visit_seq(SeqDe::new(self.fields)),
        }
    }
}
