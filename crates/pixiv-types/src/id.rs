//! Identifier newtype.

use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A Pixiv illustration or user identifier.
///
/// Pixiv is inconsistent about id encoding: `ajax/*` endpoints send strings
/// (`"illustId":"117661885"`) while `ranking.php` sends numbers. [`Id`]
/// accepts both when deserialising and always emits a string when serialising,
/// because that is the form the `ajax/*` query strings expect.
///
/// Note that ids exceed `i32`, so they must never be narrowed for the UI layer.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Id(pub u64);

impl Id {
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }
}

impl From<u64> for Id {
    fn from(v: u64) -> Self {
        Self(v)
    }
}

impl From<Id> for u64 {
    fn from(v: Id) -> Self {
        v.0
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Debug for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Id({})", self.0)
    }
}

struct IdVisitor;

impl Visitor<'_> for IdVisitor {
    type Value = Id;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a Pixiv id encoded as a string or an integer")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        v.trim()
            .parse::<u64>()
            .map(Id)
            .map_err(|_| E::custom(format!("invalid Pixiv id {v:?}")))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
        Ok(Id(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
        u64::try_from(v)
            .map(Id)
            .map_err(|_| E::custom(format!("negative Pixiv id {v}")))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
        if v >= 0.0 && v.fract() == 0.0 && v <= u64::MAX as f64 {
            Ok(Id(v as u64))
        } else {
            Err(E::custom(format!("non-integral Pixiv id {v}")))
        }
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(Id::default())
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(Id::default())
    }
}

impl<'de> Deserialize<'de> for Id {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(IdVisitor)
    }
}

impl Serialize for Id {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::Id;

    #[test]
    fn parses_string_and_numeric_forms() {
        assert_eq!(
            serde_json::from_str::<Id>("\"117661885\"").unwrap().get(),
            117_661_885
        );
        assert_eq!(
            serde_json::from_str::<Id>("117661885").unwrap().get(),
            117_661_885
        );
    }

    #[test]
    fn serialises_as_string() {
        assert_eq!(serde_json::to_string(&Id(42)).unwrap(), "\"42\"");
    }

    #[test]
    fn rejects_negative_and_garbage() {
        assert!(serde_json::from_str::<Id>("-5").is_err());
        assert!(serde_json::from_str::<Id>("\"abc\"").is_err());
    }
}
