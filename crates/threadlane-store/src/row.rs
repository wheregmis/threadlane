//! Decoding columns into domain types, and encoding domain types as parameters.

use std::str::FromStr;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value as Json;
use threadlane_core::store::{StoreError, StoreResult};
use threadlane_core::time::Timestamp;
use turso::{Row, Value};

/// Typed access to the columns of one row.
pub(crate) struct Cols<'r>(pub(crate) &'r Row);

impl Cols<'_> {
    fn value(&self, idx: usize) -> StoreResult<Value> {
        self.0
            .get_value(idx)
            .map_err(|error| StoreError::Corrupt(format!("column {idx}: {error}")))
    }

    pub(crate) fn opt_i64(&self, idx: usize) -> StoreResult<Option<i64>> {
        match self.value(idx)? {
            Value::Null => Ok(None),
            Value::Integer(value) => Ok(Some(value)),
            other => Err(StoreError::Corrupt(format!(
                "column {idx}: expected integer, found {other:?}"
            ))),
        }
    }

    pub(crate) fn i64(&self, idx: usize) -> StoreResult<i64> {
        self.opt_i64(idx)?
            .ok_or_else(|| StoreError::Corrupt(format!("column {idx}: unexpected NULL")))
    }

    pub(crate) fn u32(&self, idx: usize) -> StoreResult<u32> {
        let value = self.i64(idx)?;
        u32::try_from(value)
            .map_err(|_| StoreError::Corrupt(format!("column {idx}: {value} is not a u32")))
    }

    pub(crate) fn opt_u32(&self, idx: usize) -> StoreResult<Option<u32>> {
        self.opt_i64(idx)?
            .map(|value| {
                u32::try_from(value)
                    .map_err(|_| StoreError::Corrupt(format!("column {idx}: {value} is not a u32")))
            })
            .transpose()
    }

    pub(crate) fn bool(&self, idx: usize) -> StoreResult<bool> {
        Ok(self.i64(idx)? != 0)
    }

    pub(crate) fn id<T: From<i64>>(&self, idx: usize) -> StoreResult<T> {
        self.i64(idx).map(T::from)
    }

    pub(crate) fn opt_id<T: From<i64>>(&self, idx: usize) -> StoreResult<Option<T>> {
        Ok(self.opt_i64(idx)?.map(T::from))
    }

    pub(crate) fn ts(&self, idx: usize) -> StoreResult<Timestamp> {
        self.i64(idx).map(Timestamp::from_millis)
    }

    pub(crate) fn opt_ts(&self, idx: usize) -> StoreResult<Option<Timestamp>> {
        Ok(self.opt_i64(idx)?.map(Timestamp::from_millis))
    }

    pub(crate) fn opt_text(&self, idx: usize) -> StoreResult<Option<String>> {
        match self.value(idx)? {
            Value::Null => Ok(None),
            Value::Text(text) => Ok(Some(text)),
            other => Err(StoreError::Corrupt(format!(
                "column {idx}: expected text, found {other:?}"
            ))),
        }
    }

    pub(crate) fn text(&self, idx: usize) -> StoreResult<String> {
        self.opt_text(idx)?
            .ok_or_else(|| StoreError::Corrupt(format!("column {idx}: unexpected NULL")))
    }

    pub(crate) fn parse<T>(&self, idx: usize) -> StoreResult<T>
    where
        T: FromStr,
        T::Err: std::fmt::Display,
    {
        let text = self.text(idx)?;
        text.parse()
            .map_err(|error: T::Err| StoreError::Corrupt(format!("column {idx}: {error}")))
    }

    pub(crate) fn opt_parse<T>(&self, idx: usize) -> StoreResult<Option<T>>
    where
        T: FromStr,
        T::Err: std::fmt::Display,
    {
        self.opt_text(idx)?
            .map(|text| {
                text.parse()
                    .map_err(|error: T::Err| StoreError::Corrupt(format!("column {idx}: {error}")))
            })
            .transpose()
    }

    pub(crate) fn json(&self, idx: usize) -> StoreResult<Json> {
        self.de(idx)
    }

    pub(crate) fn opt_json(&self, idx: usize) -> StoreResult<Option<Json>> {
        self.opt_de(idx)
    }

    pub(crate) fn de<T: DeserializeOwned>(&self, idx: usize) -> StoreResult<T> {
        let text = self.text(idx)?;
        serde_json::from_str(&text)
            .map_err(|error| StoreError::Corrupt(format!("column {idx}: {error}")))
    }

    pub(crate) fn opt_de<T: DeserializeOwned>(&self, idx: usize) -> StoreResult<Option<T>> {
        self.opt_text(idx)?
            .map(|text| {
                serde_json::from_str(&text)
                    .map_err(|error| StoreError::Corrupt(format!("column {idx}: {error}")))
            })
            .transpose()
    }
}

/// Encodes a value as JSON text for a TEXT column.
pub(crate) fn json_text<T: Serialize + ?Sized>(value: &T) -> StoreResult<String> {
    serde_json::to_string(value).map_err(|error| StoreError::Invalid(error.to_string()))
}

/// Builds a parameter list.
macro_rules! params {
    ($($value:expr),* $(,)?) => {
        vec![$(::turso::Value::from($value)),*]
    };
}
pub(crate) use params;

/// Converts an optional typed id into a nullable integer parameter.
pub(crate) fn opt_raw<T>(value: Option<T>, raw: impl Fn(T) -> i64) -> Value {
    value.map_or(Value::Null, |value| Value::Integer(raw(value)))
}

/// Converts an optional timestamp into a nullable integer parameter.
pub(crate) fn opt_ts(value: Option<Timestamp>) -> Value {
    opt_raw(value, Timestamp::as_millis)
}
