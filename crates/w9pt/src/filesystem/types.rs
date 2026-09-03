//! Opaque backend handle types.

macro_rules! opaque_handle {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(u128);

        impl $name {
            /// Constructs a handle from a backend-assigned value.
            pub const fn new(value: u128) -> Self {
                Self(value)
            }

            /// Returns the opaque value for routing back to the backend that assigned it.
            pub const fn get(self) -> u128 {
                self.0
            }
        }
    };
}

opaque_handle!(
    ObjectHandle,
    "Opaque handle for a resolved filesystem object."
);
opaque_handle!(
    OpenHandle,
    "Opaque handle for an opened file or directory instance."
);
opaque_handle!(
    XattrHandle,
    "Opaque handle for an extended-attribute stream."
);

/// Host/backend principal identity bound by a successful attach.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PrincipalId(String);

impl PrincipalId {
    /// Constructs an opaque principal identifier.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrows the identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Backend export identity bound by a successful attach.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ExportId(String);

impl ExportId {
    /// Constructs an opaque export identifier.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrows the identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Authorization and routing context carried by every filesystem operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestContext {
    /// Originating transport connection.
    pub session_id: crate::protocol::SessionId,
    /// Principal selected by attach policy.
    pub principal: PrincipalId,
    /// Attached export selected by policy.
    pub export: ExportId,
}

impl RequestContext {
    /// Constructs bound request context after a successful attach.
    pub const fn new(
        session_id: crate::protocol::SessionId,
        principal: PrincipalId,
        export: ExportId,
    ) -> Self {
        Self {
            session_id,
            principal,
            export,
        }
    }
}
