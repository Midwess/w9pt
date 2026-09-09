//! Standard mode-bit and namespace authorization rules.

use core::fmt;

use w9pt_fs_state::{GroupId, InodeKind, InodeRecord, PrincipalId};

use crate::ExportGrant;

const OTHER_SHIFT: u32 = 0;
const GROUP_SHIFT: u32 = 3;
const OWNER_SHIFT: u32 = 6;
const READ_BIT: u32 = 0b100;
const WRITE_BIT: u32 = 0b010;
const EXECUTE_BIT: u32 = 0b001;
const EXECUTE_MASK: u32 = 0o111;
const STICKY_BIT: u32 = 0o1000;
const SETGID_BIT: u32 = 0o2000;

/// Requested read, write, and execute/search access to one inode.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AccessRequirements {
    /// Require the selected class's read bit.
    pub read: bool,
    /// Require the selected class's write bit.
    pub write: bool,
    /// Require the selected class's execute/search bit.
    pub execute: bool,
}

impl AccessRequirements {
    /// Read access only.
    pub const READ: Self = Self {
        read: true,
        write: false,
        execute: false,
    };
    /// Write access only.
    pub const WRITE: Self = Self {
        read: false,
        write: true,
        execute: false,
    };
    /// Execute or directory-search access only.
    pub const EXECUTE: Self = Self {
        read: false,
        write: false,
        execute: true,
    };
    /// Directory mutation access: write plus search.
    pub const DIRECTORY_MUTATION: Self = Self {
        read: false,
        write: true,
        execute: true,
    };
}

/// Group and mode inherited by a newly created inode.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreationAttributes {
    /// Canonical owning group after setgid-directory inheritance.
    pub group: GroupId,
    /// Permission/special bits after setgid-directory inheritance.
    pub mode: u32,
}

/// Stable reason an authorization rule denied semantic work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorizationError {
    /// Standard mode bits deny the requested access.
    PermissionDenied,
    /// Ownership, sticky, or group-change policy forbids the operation.
    OperationNotPermitted,
    /// A directory-specific rule was requested for another inode kind.
    NotDirectory,
    /// The resolved export forbids authoritative mutations.
    ReadOnlyExport,
    /// Requested mode contains bits outside the supported permission/special mask.
    InvalidMode,
}

impl fmt::Display for AuthorizationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "filesystem authorization failed: {self:?}")
    }
}

impl std::error::Error for AuthorizationError {}

/// Checks standard owner/group/other permission bits for one inode.
pub fn check_inode_access(
    grant: &ExportGrant,
    inode: &InodeRecord,
    required: AccessRequirements,
) -> Result<(), AuthorizationError> {
    if grant.privileged() {
        if required.execute
            && inode.kind() != InodeKind::Directory
            && inode.mode() & EXECUTE_MASK == 0
        {
            return Err(AuthorizationError::PermissionDenied);
        }
        return Ok(());
    }
    let shift = if grant.principal() == inode.owner() {
        OWNER_SHIFT
    } else if grant.belongs_to_group(inode.group()) {
        GROUP_SHIFT
    } else {
        OTHER_SHIFT
    };
    let permissions = (inode.mode() >> shift) & 0b111;
    let required_bits = (u32::from(required.read) * READ_BIT)
        | (u32::from(required.write) * WRITE_BIT)
        | (u32::from(required.execute) * EXECUTE_BIT);
    if permissions & required_bits == required_bits {
        Ok(())
    } else {
        Err(AuthorizationError::PermissionDenied)
    }
}

/// Checks execute/search access to a directory.
pub fn check_directory_search(
    grant: &ExportGrant,
    directory: &InodeRecord,
) -> Result<(), AuthorizationError> {
    require_directory(directory)?;
    check_inode_access(grant, directory, AccessRequirements::EXECUTE)
}

/// Checks read-only policy plus write/search access for a namespace mutation.
pub fn check_directory_mutation(
    grant: &ExportGrant,
    directory: &InodeRecord,
) -> Result<(), AuthorizationError> {
    check_mutation_allowed(grant)?;
    require_directory(directory)?;
    if directory.link_count() == 0 {
        return Err(AuthorizationError::OperationNotPermitted);
    }
    check_inode_access(grant, directory, AccessRequirements::DIRECTORY_MUTATION)
}

/// Rejects mutation before any content preparation when the export is read-only.
pub const fn check_mutation_allowed(grant: &ExportGrant) -> Result<(), AuthorizationError> {
    if grant.read_only() {
        Err(AuthorizationError::ReadOnlyExport)
    } else {
        Ok(())
    }
}

/// Applies the sticky-directory owner rule for unlink or rename.
pub fn check_sticky_directory(
    grant: &ExportGrant,
    directory: &InodeRecord,
    target: &InodeRecord,
) -> Result<(), AuthorizationError> {
    require_directory(directory)?;
    if directory.mode() & STICKY_BIT == 0
        || grant.privileged()
        || grant.principal() == directory.owner()
        || grant.principal() == target.owner()
    {
        Ok(())
    } else {
        Err(AuthorizationError::OperationNotPermitted)
    }
}

/// Requires ownership of an inode unless the resolved principal is privileged.
pub fn check_owner_or_privileged(
    grant: &ExportGrant,
    inode: &InodeRecord,
) -> Result<(), AuthorizationError> {
    if grant.privileged() || grant.principal() == inode.owner() {
        Ok(())
    } else {
        Err(AuthorizationError::OperationNotPermitted)
    }
}

/// Checks non-privileged owner/group changes using canonical identities.
pub fn check_ownership_change(
    grant: &ExportGrant,
    inode: &InodeRecord,
    new_owner: Option<&PrincipalId>,
    new_group: Option<&GroupId>,
) -> Result<(), AuthorizationError> {
    check_mutation_allowed(grant)?;
    if grant.privileged() {
        return Ok(());
    }
    check_owner_or_privileged(grant, inode)?;
    if new_owner.is_some_and(|owner| owner != inode.owner())
        || new_group.is_some_and(|group| !grant.belongs_to_group(group))
    {
        Err(AuthorizationError::OperationNotPermitted)
    } else {
        Ok(())
    }
}

/// Applies requested-group checks and setgid-directory inheritance for creation.
pub fn creation_attributes(
    grant: &ExportGrant,
    parent: &InodeRecord,
    requested_group: GroupId,
    requested_mode: u32,
    child_kind: InodeKind,
) -> Result<CreationAttributes, AuthorizationError> {
    require_directory(parent)?;
    if requested_mode & !0o7777 != 0 {
        return Err(AuthorizationError::InvalidMode);
    }
    let parent_setgid = parent.mode() & SETGID_BIT != 0;
    let group = if parent_setgid {
        parent.group().clone()
    } else {
        if !grant.privileged() && !grant.belongs_to_group(&requested_group) {
            return Err(AuthorizationError::OperationNotPermitted);
        }
        requested_group
    };
    let mut mode = if parent_setgid && child_kind == InodeKind::Directory {
        requested_mode | SETGID_BIT
    } else {
        requested_mode
    };
    if child_kind != InodeKind::Directory
        && mode & SETGID_BIT != 0
        && !grant.privileged()
        && !grant.belongs_to_group(&group)
    {
        mode &= !SETGID_BIT;
    }
    Ok(CreationAttributes { group, mode })
}

fn require_directory(inode: &InodeRecord) -> Result<(), AuthorizationError> {
    if inode.kind() == InodeKind::Directory {
        Ok(())
    } else {
        Err(AuthorizationError::NotDirectory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use w9pt::filesystem::CapabilitySet;
    use w9pt_fs_state::{
        DirectoryGeneration, FilesystemId, InodeData, InodeGeneration, InodeId, InodeTimes,
        QidPath, RecordRevision, StateLimits, UnixTimestamp,
    };

    fn identity(bytes: &[u8]) -> PrincipalId {
        PrincipalId::new(bytes.to_vec(), StateLimits::default()).unwrap()
    }

    fn group(bytes: &[u8]) -> GroupId {
        GroupId::new(bytes.to_vec(), StateLimits::default()).unwrap()
    }

    fn directory(owner: &[u8], group_id: &[u8], mode: u32) -> InodeRecord {
        let timestamp = UnixTimestamp::new(0, 0).unwrap();
        let inode_id = InodeId::from_u128(1);
        InodeRecord::new(
            inode_id,
            QidPath::new(1).unwrap(),
            RecordRevision::new(1).unwrap(),
            mode,
            identity(owner),
            group(group_id),
            InodeTimes {
                accessed: timestamp,
                modified: timestamp,
                changed: timestamp,
                created: timestamp,
            },
            0,
            1,
            InodeGeneration::new(1).unwrap(),
            InodeData::Directory {
                generation: DirectoryGeneration::new(1).unwrap(),
                parent_inode_id: inode_id,
            },
        )
        .unwrap()
    }

    fn grant(
        principal: &[u8],
        primary: &[u8],
        supplementary: Vec<GroupId>,
        privileged: bool,
        read_only: bool,
    ) -> ExportGrant {
        ExportGrant::new(
            FilesystemId::from_u128(1),
            InodeId::from_u128(1),
            identity(principal),
            group(primary),
            supplementary,
            1,
            1,
            privileged,
            read_only,
            1,
            CapabilitySet::NONE,
            crate::EngineLimits::default(),
        )
        .unwrap()
    }

    #[test]
    fn owner_group_supplementary_and_other_bits_are_selected() {
        let inode = directory(b"owner", b"staff", 0o750);
        assert!(
            check_directory_search(&grant(b"owner", b"x", vec![], false, false), &inode).is_ok()
        );
        assert!(
            check_directory_search(&grant(b"member", b"staff", vec![], false, false), &inode)
                .is_ok()
        );
        assert!(
            check_directory_search(
                &grant(b"member", b"x", vec![group(b"staff")], false, false),
                &inode
            )
            .is_ok()
        );
        assert_eq!(
            check_directory_search(&grant(b"other", b"x", vec![], false, false), &inode),
            Err(AuthorizationError::PermissionDenied)
        );
    }

    #[test]
    fn sticky_setgid_read_only_and_privilege_rules_are_explicit() {
        let parent = directory(b"parent", b"staff", 0o3770);
        let target = directory(b"target", b"staff", 0o700);
        assert_eq!(
            check_sticky_directory(
                &grant(b"other", b"staff", vec![], false, false),
                &parent,
                &target,
            ),
            Err(AuthorizationError::OperationNotPermitted)
        );
        assert!(
            check_sticky_directory(
                &grant(b"target", b"x", vec![], false, false),
                &parent,
                &target,
            )
            .is_ok()
        );
        let inherited = creation_attributes(
            &grant(b"child", b"users", vec![], false, false),
            &parent,
            group(b"users"),
            0o755,
            InodeKind::Directory,
        )
        .unwrap();
        assert_eq!(inherited.group, group(b"staff"));
        assert_eq!(inherited.mode, 0o2755);
        assert_eq!(
            check_directory_mutation(&grant(b"parent", b"staff", vec![], false, true), &parent,),
            Err(AuthorizationError::ReadOnlyExport)
        );
        assert!(
            check_directory_mutation(&grant(b"root", b"x", vec![], true, false), &parent,).is_ok()
        );
    }

    #[test]
    fn ownership_changes_require_owner_membership_or_privilege() {
        let inode = directory(b"owner", b"staff", 0o700);
        let owner = grant(b"owner", b"users", vec![group(b"staff")], false, false);
        assert!(check_ownership_change(&owner, &inode, None, Some(&group(b"staff"))).is_ok());
        assert_eq!(
            check_ownership_change(&owner, &inode, Some(&identity(b"other")), None),
            Err(AuthorizationError::OperationNotPermitted)
        );
        assert!(
            check_ownership_change(
                &grant(b"root", b"root", vec![], true, false),
                &inode,
                Some(&identity(b"other")),
                Some(&group(b"other")),
            )
            .is_ok()
        );
    }

    #[test]
    fn regular_file_setgid_is_cleared_without_membership_in_inherited_group() {
        let parent = directory(b"parent", b"staff", 0o2777);
        let unprivileged = creation_attributes(
            &grant(b"child", b"users", vec![], false, false),
            &parent,
            group(b"users"),
            0o2755,
            InodeKind::RegularFile,
        )
        .unwrap();
        assert_eq!(unprivileged.group, group(b"staff"));
        assert_eq!(unprivileged.mode, 0o755);

        let member = creation_attributes(
            &grant(b"child", b"users", vec![group(b"staff")], false, false),
            &parent,
            group(b"users"),
            0o2755,
            InodeKind::RegularFile,
        )
        .unwrap();
        assert_eq!(member.mode, 0o2755);

        let privileged = creation_attributes(
            &grant(b"root", b"root", vec![], true, false),
            &parent,
            group(b"root"),
            0o2755,
            InodeKind::RegularFile,
        )
        .unwrap();
        assert_eq!(privileged.mode, 0o2755);
    }
}
