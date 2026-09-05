//! Canonical full-range unsigned PostgreSQL numeric conversion.

use core::fmt;

pub(crate) fn encode_u64(value: u64) -> String {
    value.to_string()
}

pub(crate) fn decode_u64(
    field: &'static str,
    numeric_text: &str,
) -> Result<u64, NumericCodecError> {
    if numeric_text.is_empty()
        || !numeric_text.bytes().all(|byte| byte.is_ascii_digit())
        || (numeric_text.len() > 1 && numeric_text.starts_with('0'))
    {
        return Err(NumericCodecError {
            field,
            value: numeric_text.to_owned(),
        });
    }
    let value = numeric_text.parse::<u64>().map_err(|_| NumericCodecError {
        field,
        value: numeric_text.to_owned(),
    })?;
    if encode_u64(value) != numeric_text {
        return Err(NumericCodecError {
            field,
            value: numeric_text.to_owned(),
        });
    }
    Ok(value)
}

/// A PostgreSQL numeric value was not canonical full-range unsigned decimal text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NumericCodecError {
    /// Stable persisted field name.
    pub field: &'static str,
    /// Rejected textual representation.
    pub value: String,
}

impl fmt::Display for NumericCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} value {:?} is not canonical unsigned decimal",
            self.field, self.value
        )
    }
}

impl std::error::Error for NumericCodecError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_unsigned_boundaries_round_trip_canonically() {
        for value in [0, i64::MAX as u64, i64::MAX as u64 + 1, u64::MAX] {
            let encoded = encode_u64(value);
            assert_eq!(decode_u64("value", &encoded), Ok(value));
        }
    }

    #[test]
    fn noncanonical_and_out_of_range_values_are_rejected() {
        for value in [
            "",
            "00",
            "01",
            "+1",
            "-1",
            "1.0",
            "1e1",
            " 1",
            "18446744073709551616",
        ] {
            assert!(decode_u64("value", value).is_err(), "accepted {value:?}");
        }
    }
}
