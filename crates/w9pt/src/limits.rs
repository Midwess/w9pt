//! Validated bounds and retained-state accounting.

use core::fmt;

/// Minimum size of a 9P frame: size, message type, and tag.
pub const MIN_FRAME_SIZE: u32 = 7;

/// Configurable upper bounds for all attacker-amplifiable session state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Limits {
    /// Largest frame accepted before or after negotiation.
    pub max_frame_size: u32,
    /// Largest incomplete stream tail retained by a session.
    pub max_buffered_input_bytes: usize,
    /// Maximum number of effects waiting for the host.
    pub max_queued_effects: usize,
    /// Maximum total payload bytes retained by queued effects.
    pub max_queued_effect_bytes: usize,
    /// Maximum simultaneously active non-version request tags.
    pub max_in_flight_tags: usize,
    /// Maximum installed fids.
    pub max_fids: usize,
    /// Maximum path components in one walk request.
    pub max_walk_elements: usize,
    /// Maximum bytes in one protocol string.
    pub max_string_bytes: usize,
    /// Maximum write payload bytes retained awaiting completion.
    pub max_pending_write_bytes: usize,
}

impl Limits {
    /// Validates that limits are non-zero and consistent with wire representations.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidLimits`] naming the first invalid field.
    pub fn validate(&self) -> Result<(), InvalidLimits> {
        if self.max_frame_size < MIN_FRAME_SIZE {
            return Err(InvalidLimits::TooSmall("max_frame_size"));
        }
        for (name, value) in [
            ("max_buffered_input_bytes", self.max_buffered_input_bytes),
            ("max_queued_effects", self.max_queued_effects),
            ("max_queued_effect_bytes", self.max_queued_effect_bytes),
            ("max_in_flight_tags", self.max_in_flight_tags),
            ("max_fids", self.max_fids),
            ("max_walk_elements", self.max_walk_elements),
            ("max_string_bytes", self.max_string_bytes),
            ("max_pending_write_bytes", self.max_pending_write_bytes),
        ] {
            if value == 0 {
                return Err(InvalidLimits::TooSmall(name));
            }
        }
        if self.max_buffered_input_bytes < self.max_frame_size as usize {
            return Err(InvalidLimits::Inconsistent {
                smaller: "max_buffered_input_bytes",
                larger: "max_frame_size",
            });
        }
        if self.max_string_bytes > u16::MAX as usize {
            return Err(InvalidLimits::TooLarge("max_string_bytes"));
        }
        if self.max_walk_elements > u16::MAX as usize {
            return Err(InvalidLimits::TooLarge("max_walk_elements"));
        }
        Ok(())
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_frame_size: 1024 * 1024,
            max_buffered_input_bytes: 1024 * 1024,
            max_queued_effects: 256,
            max_queued_effect_bytes: 4 * 1024 * 1024,
            max_in_flight_tags: 128,
            max_fids: 4096,
            max_walk_elements: 16,
            max_string_bytes: 4096,
            max_pending_write_bytes: 8 * 1024 * 1024,
        }
    }
}

/// Configuration validation failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvalidLimits {
    /// A field is below its usable minimum.
    TooSmall(&'static str),
    /// A field cannot be represented on the wire.
    TooLarge(&'static str),
    /// A containing bound is smaller than a bound it must contain.
    Inconsistent {
        /// The containing field that is too small.
        smaller: &'static str,
        /// The field that cannot fit within it.
        larger: &'static str,
    },
}

impl fmt::Display for InvalidLimits {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooSmall(field) => write!(formatter, "{field} is too small"),
            Self::TooLarge(field) => write!(formatter, "{field} is too large"),
            Self::Inconsistent { smaller, larger } => {
                write!(formatter, "{smaller} cannot contain {larger}")
            }
        }
    }
}

impl std::error::Error for InvalidLimits {}

/// Resource whose configured session bound was reached.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LimitKind {
    /// Incomplete input bytes.
    BufferedInput,
    /// Complete frame bytes.
    Frame,
    /// Effects awaiting polling.
    QueuedEffects,
    /// Bytes retained in queued effect payloads.
    QueuedEffectBytes,
    /// Active request tags.
    InFlightTags,
    /// Installed fids.
    Fids,
    /// Elements in one walk.
    WalkElements,
    /// Bytes in one protocol string.
    StringBytes,
    /// Write bytes retained while awaiting completions.
    PendingWriteBytes,
}

/// A checked accounting operation would exceed its configured bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LimitExceeded {
    /// Resource that reached its bound.
    pub kind: LimitKind,
    /// Configured maximum.
    pub limit: usize,
    /// Requested total after the operation.
    pub requested: usize,
}

impl fmt::Display for LimitExceeded {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:?} limit exceeded: requested {}, limit {}",
            self.kind, self.requested, self.limit
        )
    }
}

impl std::error::Error for LimitExceeded {}

/// Mutable counters for retained session state.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Accounting {
    pub queued_effects: usize,
    pub queued_effect_bytes: usize,
    pub in_flight_tags: usize,
    pub fids: usize,
    pub pending_write_bytes: usize,
}

impl Accounting {
    pub fn reserve(
        current: &mut usize,
        amount: usize,
        limit: usize,
        kind: LimitKind,
    ) -> Result<(), LimitExceeded> {
        let requested = current.checked_add(amount).ok_or(LimitExceeded {
            kind,
            limit,
            requested: usize::MAX,
        })?;
        if requested > limit {
            return Err(LimitExceeded {
                kind,
                limit,
                requested,
            });
        }
        *current = requested;
        Ok(())
    }

    pub fn release(current: &mut usize, amount: usize) {
        *current = current.saturating_sub(amount);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_reject_unrepresentable_strings() {
        let limits = Limits {
            max_string_bytes: usize::from(u16::MAX) + 1,
            ..Limits::default()
        };
        assert_eq!(
            limits.validate(),
            Err(InvalidLimits::TooLarge("max_string_bytes"))
        );
    }

    #[test]
    fn accounting_detects_overflow_and_does_not_mutate() {
        let mut current = 5;
        let result = Accounting::reserve(&mut current, 4, 8, LimitKind::Fids);
        assert_eq!(current, 5);
        assert_eq!(result.unwrap_err().requested, 9);
    }
}
