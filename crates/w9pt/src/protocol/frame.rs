//! Bounded incremental stream framing.

use crate::{error::DecodeError, limits::Limits};

use super::HEADER_SIZE;

/// Retains at most one incomplete frame tail between stream input calls.
#[derive(Clone, Debug)]
pub struct FrameDecoder {
    retained: Vec<u8>,
    hard_maximum: u32,
    maximum_retained: usize,
}

impl FrameDecoder {
    /// Creates a decoder from validated session limits.
    pub fn new(limits: &Limits) -> Self {
        Self {
            retained: Vec::new(),
            hard_maximum: limits.max_frame_size,
            maximum_retained: limits.max_buffered_input_bytes,
        }
    }

    /// Returns the incomplete bytes currently retained.
    pub fn retained_len(&self) -> usize {
        self.retained.len()
    }

    /// Discards an incomplete tail, for version reset or terminal closure.
    pub fn clear(&mut self) {
        self.retained.clear();
    }

    /// Consumes an arbitrary stream chunk and returns every complete frame exactly once.
    ///
    /// `active_maximum` is the negotiated `msize`, or the configured hard maximum before
    /// negotiation. The smaller of it and the construction-time hard maximum is enforced before
    /// any declared frame body is retained.
    ///
    /// # Errors
    ///
    /// Returns a typed framing error and clears the incomplete tail when a size is invalid.
    pub fn push(
        &mut self,
        mut input: &[u8],
        active_maximum: u32,
    ) -> Result<Vec<Vec<u8>>, DecodeError> {
        let maximum = self.hard_maximum.min(active_maximum);
        let mut frames = Vec::new();

        while !input.is_empty() {
            if self.retained.is_empty() && input.len() >= 4 {
                let declared = u32::from_le_bytes(input[..4].try_into().expect("checked length"));
                self.validate_declared(declared, maximum)?;
                let frame_size = declared as usize;
                if input.len() >= frame_size {
                    frames.push(input[..frame_size].to_vec());
                    input = &input[frame_size..];
                    continue;
                }
                self.retain(input)?;
                break;
            }

            if self.retained.len() < 4 {
                let header_needed = 4 - self.retained.len();
                let consumed = header_needed.min(input.len());
                self.retain(&input[..consumed])?;
                input = &input[consumed..];
                if self.retained.len() < 4 {
                    break;
                }
            }

            let declared = u32::from_le_bytes(
                self.retained[..4]
                    .try_into()
                    .expect("retained header is complete"),
            );
            if let Err(error) = self.validate_declared(declared, maximum) {
                self.retained.clear();
                return Err(error);
            }
            let frame_size = declared as usize;
            let needed = frame_size - self.retained.len();
            let consumed = needed.min(input.len());
            self.retain(&input[..consumed])?;
            input = &input[consumed..];
            if self.retained.len() == frame_size {
                frames.push(core::mem::take(&mut self.retained));
            }
        }

        Ok(frames)
    }

    fn validate_declared(&self, declared: u32, maximum: u32) -> Result<(), DecodeError> {
        if declared < HEADER_SIZE as u32 {
            return Err(DecodeError::FrameTooSmall { size: declared });
        }
        if declared > maximum {
            return Err(DecodeError::FrameTooLarge {
                size: declared,
                maximum,
            });
        }
        if declared as usize > self.maximum_retained {
            return Err(DecodeError::FrameTooLarge {
                size: declared,
                maximum: u32::try_from(self.maximum_retained).unwrap_or(u32::MAX),
            });
        }
        Ok(())
    }

    fn retain(&mut self, bytes: &[u8]) -> Result<(), DecodeError> {
        let requested = self
            .retained
            .len()
            .checked_add(bytes.len())
            .ok_or(DecodeError::ArithmeticOverflow)?;
        if requested > self.maximum_retained {
            self.retained.clear();
            return Err(DecodeError::FrameTooLarge {
                size: u32::try_from(requested).unwrap_or(u32::MAX),
                maximum: u32::try_from(self.maximum_retained).unwrap_or(u32::MAX),
            });
        }
        self.retained.extend_from_slice(bytes);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(tag: u16) -> Vec<u8> {
        vec![7, 0, 0, 0, 108, tag as u8, (tag >> 8) as u8]
    }

    #[test]
    fn fragmented_and_coalesced_frames_are_emitted_once() {
        let mut decoder = FrameDecoder::new(&Limits::default());
        let one = frame(1);
        let two = frame(2);
        assert!(decoder.push(&one[..3], 1024).unwrap().is_empty());
        let mut tail = one[3..].to_vec();
        tail.extend_from_slice(&two);
        assert_eq!(decoder.push(&tail, 1024).unwrap(), vec![one, two]);
        assert_eq!(decoder.retained_len(), 0);
    }

    #[test]
    fn oversized_header_is_rejected_before_body_retention() {
        let mut decoder = FrameDecoder::new(&Limits::default());
        let result = decoder.push(&2048u32.to_le_bytes(), 1024);
        assert_eq!(
            result,
            Err(DecodeError::FrameTooLarge {
                size: 2048,
                maximum: 1024
            })
        );
        assert_eq!(decoder.retained_len(), 0);
    }
}
