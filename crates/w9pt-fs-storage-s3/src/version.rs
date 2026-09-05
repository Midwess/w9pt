//! Canonical opaque ETag version tokens.

use w9pt_fs_storage::ObjectVersion;

use crate::S3Error;

const ADAPTER_TAG: u8 = b'S';
const TOKEN_VERSION: u8 = 1;
const HEADER_BYTES: usize = 4;

pub(crate) fn encode_etag(etag: &str, maximum: usize) -> Result<ObjectVersion, S3Error> {
    validate_etag(etag, maximum)?;
    let length = u16::try_from(etag.len()).map_err(|_| S3Error::InvalidVersion {
        reason: "ETag length is not representable",
    })?;
    let capacity = HEADER_BYTES
        .checked_add(etag.len())
        .ok_or(S3Error::InvalidVersion {
            reason: "token length overflow",
        })?;
    let mut bytes = Vec::with_capacity(capacity);
    bytes.push(ADAPTER_TAG);
    bytes.push(TOKEN_VERSION);
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(etag.as_bytes());
    Ok(ObjectVersion::new(bytes))
}

pub(crate) fn decode_etag(version: &ObjectVersion, maximum: usize) -> Result<&str, S3Error> {
    let bytes = version.as_bytes();
    if bytes.len() < HEADER_BYTES {
        return Err(S3Error::InvalidVersion {
            reason: "token is truncated",
        });
    }
    if bytes[0] != ADAPTER_TAG {
        return Err(S3Error::InvalidVersion {
            reason: "foreign adapter tag",
        });
    }
    if bytes[1] != TOKEN_VERSION {
        return Err(S3Error::InvalidVersion {
            reason: "unsupported token version",
        });
    }
    let encoded_length = usize::from(u16::from_be_bytes([bytes[2], bytes[3]]));
    let expected_length =
        HEADER_BYTES
            .checked_add(encoded_length)
            .ok_or(S3Error::InvalidVersion {
                reason: "token length overflow",
            })?;
    if bytes.len() != expected_length {
        return Err(S3Error::InvalidVersion {
            reason: "inconsistent length or trailing bytes",
        });
    }
    let etag =
        core::str::from_utf8(&bytes[HEADER_BYTES..]).map_err(|_| S3Error::InvalidVersion {
            reason: "ETag is not UTF-8",
        })?;
    validate_etag(etag, maximum)?;
    Ok(etag)
}

fn validate_etag(etag: &str, maximum: usize) -> Result<(), S3Error> {
    if etag.is_empty() {
        return Err(S3Error::InvalidVersion {
            reason: "ETag is empty",
        });
    }
    if etag.len() > maximum {
        return Err(S3Error::InvalidVersion {
            reason: "ETag exceeds configured bound",
        });
    }
    let bytes = etag.as_bytes();
    if bytes.len() <= 2 || bytes.first() != Some(&b'"') || bytes.last() != Some(&b'"') {
        return Err(S3Error::InvalidVersion {
            reason: "ETag is not canonically quoted",
        });
    }
    if bytes[1..bytes.len() - 1]
        .iter()
        .any(|byte| *byte == b'"' || *byte < 0x21 || *byte > 0x7e)
    {
        return Err(S3Error::InvalidVersion {
            reason: "ETag contains unsupported bytes",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_token_preserves_exact_etag() {
        let version = encode_etag("\"AbC-123\"", 64).unwrap();
        assert_eq!(
            version.as_bytes(),
            &[
                b'S', 1, 0, 9, b'"', b'A', b'b', b'C', b'-', b'1', b'2', b'3', b'"'
            ]
        );
        assert_eq!(decode_etag(&version, 64).unwrap(), "\"AbC-123\"");
    }

    #[test]
    fn malformed_foreign_and_noncanonical_tokens_fail_closed() {
        for etag in [
            "",
            "\"\"",
            "unquoted",
            "W/\"weak\"",
            "\"embedded\"quote\"",
            "\"a\n\"",
        ] {
            assert!(encode_etag(etag, 64).is_err(), "{etag:?}");
        }
        assert!(encode_etag("\"too-long\"", 4).is_err());

        for bytes in [
            vec![],
            vec![b'X', 1, 0, 2, b'"', b'"'],
            vec![b'S', 2, 0, 2, b'"', b'"'],
            vec![b'S', 1, 0, 3, b'"', b'"'],
            vec![b'S', 1, 0, 1, b'"', b'"'],
            vec![b'S', 1, 0, 2, 0xff, b'"'],
            vec![b'S', 1, 0, 2, b'"', b'"'],
        ] {
            assert!(decode_etag(&ObjectVersion::new(bytes), 64).is_err());
        }
    }
}
