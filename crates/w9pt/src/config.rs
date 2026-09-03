//! Session configuration and caller-supplied connection context.

use crate::{limits::Limits, protocol::SessionId};

/// Configuration copied into one independently owned session.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SessionConfig {
    /// Hard resource limits enforced before and after version negotiation.
    pub limits: Limits,
}

impl SessionConfig {
    /// Creates a configuration after validating every resource limit.
    ///
    /// # Errors
    ///
    /// Returns an error when a limit is zero, internally inconsistent, or cannot be represented
    /// by the `9P2000.L` wire format.
    pub fn new(limits: Limits) -> Result<Self, crate::limits::InvalidLimits> {
        limits.validate()?;
        Ok(Self { limits })
    }
}

/// Immutable host-supplied metadata for one connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionContext {
    /// Globally routable session identifier.
    pub session_id: SessionId,
    /// Optional identity asserted by the transport; policy still decides whether to trust it.
    pub transport_principal: Option<String>,
}

impl SessionContext {
    /// Creates context for a session without a transport-supplied identity.
    pub const fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            transport_principal: None,
        }
    }

    /// Adds an identity asserted by the embedding transport.
    pub fn with_transport_principal(mut self, principal: impl Into<String>) -> Self {
        self.transport_principal = Some(principal.into());
        self
    }
}
