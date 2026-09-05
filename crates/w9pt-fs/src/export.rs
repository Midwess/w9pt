//! Caller-owned export, credential, and access-policy contracts.

use core::{fmt, future::Future};

use w9pt::filesystem::{CapabilitySet, RequestContext};
use w9pt_fs_state::{FilesystemId, GroupId, InodeId, PrincipalId};

use crate::{EngineLimitError, EngineLimits};

/// Owned request to resolve one attach-bound protocol context into an export grant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportPolicyRequest {
    /// Principal, export, and session identity bound by `w9pt` attach processing.
    pub context: RequestContext,
}

impl ExportPolicyRequest {
    /// Constructs a policy request from the owned filesystem request context.
    pub const fn new(context: RequestContext) -> Self {
        Self { context }
    }
}

/// Canonical state identity whose numeric 9P view is requested.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CanonicalIdentity {
    /// Canonical principal/owner identity.
    Principal(PrincipalId),
    /// Canonical group identity.
    Group(GroupId),
}

/// Owned request to map a canonical state identity to a numeric wire identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdentityMappingRequest {
    /// Filesystem whose export policy owns the mapping.
    pub filesystem_id: FilesystemId,
    /// Exact policy generation against which the mapping is resolved.
    pub policy_generation: u64,
    /// Canonical principal or group to map.
    pub identity: CanonicalIdentity,
}

/// Numeric 9P identity returned by the caller-owned mapping policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NumericIdentity {
    /// Numeric user identity.
    User(u32),
    /// Numeric group identity.
    Group(u32),
}

/// Owned request to map one numeric 9P identity into canonical state identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReverseIdentityMappingRequest {
    /// Filesystem whose export policy owns the mapping.
    pub filesystem_id: FilesystemId,
    /// Exact policy generation against which the mapping is resolved.
    pub policy_generation: u64,
    /// Numeric user or group identity supplied by the protocol request.
    pub identity: NumericIdentity,
}

impl ReverseIdentityMappingRequest {
    /// Constructs an owned reverse-mapping request.
    pub const fn new(
        filesystem_id: FilesystemId,
        policy_generation: u64,
        identity: NumericIdentity,
    ) -> Self {
        Self {
            filesystem_id,
            policy_generation,
            identity,
        }
    }
}

/// Complete owned authorization context for one resolved export and principal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportGrant {
    filesystem_id: FilesystemId,
    root_inode_id: InodeId,
    principal: PrincipalId,
    primary_group: GroupId,
    supplementary_groups: Box<[GroupId]>,
    numeric_uid: u32,
    numeric_gid: u32,
    privileged: bool,
    read_only: bool,
    policy_generation: u64,
    capability_ceiling: CapabilitySet,
}

impl ExportGrant {
    /// Constructs a bounded grant with an exact nonzero policy generation.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        filesystem_id: FilesystemId,
        root_inode_id: InodeId,
        principal: PrincipalId,
        primary_group: GroupId,
        supplementary_groups: impl Into<Vec<GroupId>>,
        numeric_uid: u32,
        numeric_gid: u32,
        privileged: bool,
        read_only: bool,
        policy_generation: u64,
        capability_ceiling: CapabilitySet,
        limits: EngineLimits,
    ) -> Result<Self, InvalidExportGrant> {
        if policy_generation == 0 {
            return Err(InvalidExportGrant::ZeroPolicyGeneration);
        }
        let mut supplementary_groups = supplementary_groups.into();
        limits
            .check_supplementary_groups(supplementary_groups.len())
            .map_err(InvalidExportGrant::Limit)?;
        supplementary_groups.sort();
        supplementary_groups.dedup();
        supplementary_groups.retain(|group| group != &primary_group);
        Ok(Self {
            filesystem_id,
            root_inode_id,
            principal,
            primary_group,
            supplementary_groups: supplementary_groups.into_boxed_slice(),
            numeric_uid,
            numeric_gid,
            privileged,
            read_only,
            policy_generation,
            capability_ceiling,
        })
    }

    /// Returns the authoritative filesystem identity.
    pub const fn filesystem_id(&self) -> FilesystemId {
        self.filesystem_id
    }

    /// Returns the export-confined root inode.
    pub const fn root_inode_id(&self) -> InodeId {
        self.root_inode_id
    }

    /// Returns the canonical requesting principal.
    pub const fn principal(&self) -> &PrincipalId {
        &self.principal
    }

    /// Returns the canonical primary group.
    pub const fn primary_group(&self) -> &GroupId {
        &self.primary_group
    }

    /// Returns bounded canonical supplementary groups.
    pub const fn supplementary_groups(&self) -> &[GroupId] {
        &self.supplementary_groups
    }

    /// Returns the requester's numeric user identity.
    pub const fn numeric_uid(&self) -> u32 {
        self.numeric_uid
    }

    /// Returns the requester's numeric primary group identity.
    pub const fn numeric_gid(&self) -> u32 {
        self.numeric_gid
    }

    /// Reports whether privileged ownership/permission rules apply.
    pub const fn privileged(&self) -> bool {
        self.privileged
    }

    /// Reports whether all state-changing operations are denied.
    pub const fn read_only(&self) -> bool {
        self.read_only
    }

    /// Returns the exact policy generation used by authorization.
    pub const fn policy_generation(&self) -> u64 {
        self.policy_generation
    }

    /// Returns the maximum capability set this export policy permits.
    pub const fn capability_ceiling(&self) -> CapabilitySet {
        self.capability_ceiling
    }

    /// Reports whether the requester belongs to a canonical inode group.
    pub fn belongs_to_group(&self, group: &GroupId) -> bool {
        &self.primary_group == group || self.supplementary_groups.contains(group)
    }
}

/// Structurally invalid caller-provided export grant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidExportGrant {
    /// Policy generations are nonzero and monotonically advanced by state.
    ZeroPolicyGeneration,
    /// The supplementary-group list exceeded the engine bound.
    Limit(EngineLimitError),
}

impl fmt::Display for InvalidExportGrant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroPolicyGeneration => formatter.write_str("policy generation must be nonzero"),
            Self::Limit(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for InvalidExportGrant {}

/// Runtime-neutral caller-owned export and numeric-identity policy.
pub trait ExportPolicy: Send + Sync {
    /// Policy/provider failure retained for host diagnostics.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Resolves an owned attach-bound request into a complete export grant.
    fn resolve(
        &self,
        request: ExportPolicyRequest,
    ) -> impl Future<Output = Result<ExportGrant, Self::Error>> + Send;

    /// Maps one owned canonical state identity to its numeric 9P view.
    fn map_numeric_identity(
        &self,
        request: IdentityMappingRequest,
    ) -> impl Future<Output = Result<NumericIdentity, Self::Error>> + Send;

    /// Maps one owned numeric 9P identity to its canonical state identity.
    ///
    /// Mutation planners use this direction for numeric uid/gid request operands before
    /// authorization and protect the decision with the request's policy generation.
    fn map_canonical_identity(
        &self,
        request: ReverseIdentityMappingRequest,
    ) -> impl Future<Output = Result<CanonicalIdentity, Self::Error>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grants_check_generation_groups_and_membership() {
        let state_limits = w9pt_fs_state::StateLimits::default();
        let primary = GroupId::new(b"primary".to_vec(), state_limits).unwrap();
        let secondary = GroupId::new(b"secondary".to_vec(), state_limits).unwrap();
        let principal = PrincipalId::new(b"principal".to_vec(), state_limits).unwrap();
        let grant = ExportGrant::new(
            FilesystemId::from_u128(1),
            InodeId::from_u128(2),
            principal,
            primary.clone(),
            vec![secondary.clone()],
            10,
            20,
            false,
            false,
            1,
            CapabilitySet::NONE,
            EngineLimits::default(),
        )
        .unwrap();
        assert!(grant.belongs_to_group(&primary));
        assert!(grant.belongs_to_group(&secondary));
        assert!(!grant.privileged());
        assert!(matches!(
            ExportGrant::new(
                FilesystemId::from_u128(1),
                InodeId::from_u128(2),
                grant.principal().clone(),
                primary,
                Vec::new(),
                10,
                20,
                false,
                false,
                0,
                CapabilitySet::NONE,
                EngineLimits::default(),
            ),
            Err(InvalidExportGrant::ZeroPolicyGeneration)
        ));
    }

    #[test]
    fn reverse_mapping_requests_own_numeric_user_and_group_inputs() {
        let filesystem_id = FilesystemId::from_u128(1);
        assert_eq!(
            ReverseIdentityMappingRequest::new(filesystem_id, 7, NumericIdentity::User(11)),
            ReverseIdentityMappingRequest {
                filesystem_id,
                policy_generation: 7,
                identity: NumericIdentity::User(11),
            }
        );
        assert_eq!(
            ReverseIdentityMappingRequest::new(filesystem_id, 8, NumericIdentity::Group(12))
                .identity,
            NumericIdentity::Group(12)
        );
    }
}
