use crate::RepresentationError;

use super::{ActualCodec, CompressionPolicy};

#[cfg(feature = "compression-lz4")]
pub(super) const MINIMUM_SAVINGS: usize = 64;

pub(super) fn encode(
    policy: CompressionPolicy,
    plaintext: &[u8],
) -> Result<(ActualCodec, Vec<u8>), RepresentationError> {
    match policy {
        CompressionPolicy::Identity => Ok((ActualCodec::Identity, plaintext.to_vec())),
        CompressionPolicy::Lz4BlockV1 => encode_lz4(plaintext),
    }
}

pub(super) fn maximum_encode_capacity(
    policy: CompressionPolicy,
    plaintext_len: usize,
) -> Result<usize, RepresentationError> {
    match policy {
        CompressionPolicy::Identity => Ok(plaintext_len),
        CompressionPolicy::Lz4BlockV1 => maximum_lz4_capacity(plaintext_len),
    }
}

#[cfg(feature = "compression-lz4")]
fn maximum_lz4_capacity(plaintext_len: usize) -> Result<usize, RepresentationError> {
    Ok(lz4_flex::block::get_maximum_output_size(plaintext_len))
}

#[cfg(not(feature = "compression-lz4"))]
fn maximum_lz4_capacity(_plaintext_len: usize) -> Result<usize, RepresentationError> {
    Err(RepresentationError::UnsupportedCompression)
}

#[cfg(feature = "compression-lz4")]
fn encode_lz4(plaintext: &[u8]) -> Result<(ActualCodec, Vec<u8>), RepresentationError> {
    let maximum = lz4_flex::block::get_maximum_output_size(plaintext.len());
    let mut encoded = vec![0; maximum];
    let written = lz4_flex::block::compress_into(plaintext, &mut encoded)
        .map_err(|_| RepresentationError::CodecFailure)?;
    encoded.truncate(written);
    if encoded.len().saturating_add(MINIMUM_SAVINGS) <= plaintext.len() {
        Ok((ActualCodec::Lz4BlockV1, encoded))
    } else {
        Ok((ActualCodec::Identity, plaintext.to_vec()))
    }
}

#[cfg(not(feature = "compression-lz4"))]
fn encode_lz4(_plaintext: &[u8]) -> Result<(ActualCodec, Vec<u8>), RepresentationError> {
    Err(RepresentationError::UnsupportedCompression)
}

pub(super) fn decode(
    codec: ActualCodec,
    encoded: &[u8],
    expected_len: usize,
) -> Result<Vec<u8>, RepresentationError> {
    match codec {
        ActualCodec::Identity => {
            if encoded.len() != expected_len {
                return Err(RepresentationError::InvalidLength);
            }
            Ok(encoded.to_vec())
        }
        ActualCodec::Lz4BlockV1 => decode_lz4(encoded, expected_len),
    }
}

#[cfg(feature = "compression-lz4")]
fn decode_lz4(encoded: &[u8], expected_len: usize) -> Result<Vec<u8>, RepresentationError> {
    let mut plaintext = vec![0; expected_len];
    let written = lz4_flex::block::decompress_into(encoded, &mut plaintext)
        .map_err(|_| RepresentationError::CodecFailure)?;
    if written != expected_len {
        return Err(RepresentationError::InvalidLength);
    }
    Ok(plaintext)
}

#[cfg(not(feature = "compression-lz4"))]
fn decode_lz4(_encoded: &[u8], _expected_len: usize) -> Result<Vec<u8>, RepresentationError> {
    Err(RepresentationError::UnsupportedCompression)
}

#[cfg(all(test, feature = "compression-lz4"))]
mod tests {
    use super::*;

    #[test]
    fn frozen_encoder_selects_lz4_at_threshold_and_rejects_trailing_input() {
        let input = vec![b'A'; 1_024];
        let (codec, encoded) = encode(CompressionPolicy::Lz4BlockV1, &input).unwrap();
        assert_eq!(
            encoded
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
            "1f410100ffffffe960414141414141"
        );
        assert_eq!(codec, ActualCodec::Lz4BlockV1);
        assert_eq!(decode(codec, &encoded, input.len()).unwrap(), input);
        let mut trailing = encoded.clone();
        trailing.push(0);
        assert!(decode(codec, &trailing, input.len()).is_err());
        for length in 0..encoded.len() {
            assert!(decode(codec, &encoded[..length], input.len()).is_err());
        }

        let noisy = (0_u32..1_024)
            .map(|index| (index.wrapping_mul(73) ^ (index >> 3)) as u8)
            .collect::<Vec<_>>();
        let (codec, encoded) = encode(CompressionPolicy::Lz4BlockV1, &noisy).unwrap();
        if encoded.len().saturating_add(MINIMUM_SAVINGS) > noisy.len() {
            assert_eq!(codec, ActualCodec::Identity);
        }
        assert_eq!(decode(codec, &encoded, noisy.len()).unwrap(), noisy);
    }
}
