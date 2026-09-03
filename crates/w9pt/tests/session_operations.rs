#![allow(missing_docs)]

mod support;

use support::{ModelDriver, frame, wire_string};
use w9pt::{
    Completion, Effect, LinuxErrno, PolicyResult, Qid,
    filesystem::{
        AttachResult, CapabilitySet, CreateResult, ExportId, FilesystemError,
        FilesystemOperation as Op, FilesystemResult as ResultValue, NodeResult, ObjectHandle,
        OpenHandle, OpenResult, PrincipalId, RequestContext, WalkElement, WalkResult,
        XattrCreateResult, XattrHandle, XattrWalkResult,
    },
    protocol::{
        DirectoryEntry, FileAttributes, GetattrMask, Lock, LockFlags, LockRequest, LockStatus,
        LockType, OpenFlags, QidType, SetAttributes, Statfs, UnlinkFlags, XattrFlags,
    },
};

fn negotiate(driver: &mut ModelDriver) {
    let mut payload = 4096u32.to_le_bytes().to_vec();
    payload.extend(wire_string("9P2000.L"));
    driver.input(frame(100, u16::MAX, &payload)).unwrap();
    assert!(matches!(driver.effect(), Some(Effect::SendFrame { .. })));
}

fn attach(driver: &mut ModelDriver, capabilities: CapabilitySet) {
    let mut payload = 1u32.to_le_bytes().to_vec();
    payload.extend(u32::MAX.to_le_bytes());
    payload.extend(wire_string("user"));
    payload.extend(wire_string("/"));
    payload.extend(1000u32.to_le_bytes());
    driver.input(frame(104, 1, &payload)).unwrap();
    let Some(Effect::Policy { operation_id, .. }) = driver.effect() else {
        panic!("expected attach policy effect")
    };
    driver
        .complete(Completion::Policy {
            operation_id,
            result: Ok(PolicyResult::Attached(AttachResult {
                principal: PrincipalId::new("p"),
                export: ExportId::new("e"),
                root: ObjectHandle::new(1),
                qid: Qid::new(QidType::DIRECTORY, 0, 1),
                capabilities,
            })),
        })
        .unwrap();
    assert!(matches!(driver.effect(), Some(Effect::SendFrame { .. })));
}

#[test]
fn deterministic_driver_replays_identical_trace() {
    fn run() -> Vec<support::ModelEvent> {
        let mut driver = ModelDriver::new(9);
        negotiate(&mut driver);
        attach(&mut driver, CapabilitySet::NONE);
        driver.trace
    }

    assert_eq!(run(), run());
}

#[test]
fn capability_rejection_emits_rlerror_without_filesystem_work() {
    let mut driver = ModelDriver::new(1);
    negotiate(&mut driver);
    attach(&mut driver, CapabilitySet::NONE);
    driver.input(frame(8, 2, &1u32.to_le_bytes())).unwrap();
    let Some(Effect::SendFrame { bytes }) = driver.effect() else {
        panic!("expected immediate Rlerror")
    };
    assert_eq!(bytes[4], 7);
    assert_eq!(
        u32::from_le_bytes(bytes[7..11].try_into().unwrap()),
        LinuxErrno::EOPNOTSUPP.get()
    );
    assert_eq!(driver.effect(), None);
}

#[test]
fn filesystem_error_maps_to_original_tag_and_stable_errno() {
    let mut driver = ModelDriver::new(1);
    negotiate(&mut driver);
    attach(&mut driver, CapabilitySet::ALL);
    driver.input(frame(8, 9, &1u32.to_le_bytes())).unwrap();
    let Some(Effect::Filesystem { operation_id, .. }) = driver.effect() else {
        panic!()
    };
    driver
        .complete(Completion::Filesystem {
            operation_id,
            result: Err(FilesystemError::new(LinuxErrno::ENOSPC)),
        })
        .unwrap();
    let Some(Effect::SendFrame { bytes }) = driver.effect() else {
        panic!()
    };
    assert_eq!(u16::from_le_bytes([bytes[5], bytes[6]]), 9);
    assert_eq!(u32::from_le_bytes(bytes[7..11].try_into().unwrap()), 28);
}

#[test]
fn every_filesystem_operation_has_exact_result_and_capability_classification() {
    let context = RequestContext::new(
        w9pt::SessionId::new(1),
        PrincipalId::new("p"),
        ExportId::new("e"),
    );
    let object = ObjectHandle::new(1);
    let open = OpenHandle::new(2);
    let xattr = XattrHandle::new(3);
    let qid = Qid::new(QidType::FILE, 0, 1);
    let lock = Lock {
        ty: LockType::Unlock,
        start: 0,
        length: 0,
        process_id: 1,
        client_id: "client".into(),
    };
    let lock_request = LockRequest {
        lock: lock.clone(),
        flags: LockFlags::EMPTY,
    };
    let node = NodeResult { object, qid };

    let pairs = vec![
        (
            Op::Walk {
                start: object,
                names: vec!["a".into()],
            },
            ResultValue::Walked(WalkResult {
                elements: vec![WalkElement { object, qid }],
            }),
        ),
        (
            Op::Release {
                object,
                open: Some(open),
                xattr: None,
            },
            ResultValue::Released,
        ),
        (
            Op::Open {
                object,
                flags: OpenFlags::RDONLY,
            },
            ResultValue::Opened(OpenResult {
                qid,
                open,
                io_unit: 0,
            }),
        ),
        (
            Op::Create {
                directory: object,
                name: "a".into(),
                flags: OpenFlags::RDWR,
                mode: 0o644,
                gid: 1,
            },
            ResultValue::Created(CreateResult {
                object,
                qid,
                open,
                io_unit: 0,
            }),
        ),
        (
            Op::Mkdir {
                directory: object,
                name: "d".into(),
                mode: 0o755,
                gid: 1,
            },
            ResultValue::DirectoryCreated(node),
        ),
        (
            Op::Mknod {
                directory: object,
                name: "n".into(),
                mode: 0,
                major: 1,
                minor: 2,
                gid: 1,
            },
            ResultValue::NodeCreated(node),
        ),
        (
            Op::Symlink {
                directory: object,
                name: "s".into(),
                target: "t".into(),
                gid: 1,
            },
            ResultValue::SymlinkCreated(node),
        ),
        (
            Op::Read {
                open,
                offset: 0,
                count: 1,
            },
            ResultValue::Read(vec![1]),
        ),
        (
            Op::Write {
                open,
                offset: 0,
                data: vec![1],
            },
            ResultValue::Written(1),
        ),
        (
            Op::ReadDir {
                open,
                offset: 0,
                count: 64,
            },
            ResultValue::DirectoryRead(vec![DirectoryEntry {
                qid,
                offset: 1,
                ty: 8,
                name: "a".into(),
            }]),
        ),
        (
            Op::Fsync {
                open,
                data_only: false,
            },
            ResultValue::Synced,
        ),
        (
            Op::Statfs { object },
            ResultValue::Statfs(Statfs::default()),
        ),
        (
            Op::Getattr {
                object,
                mask: GetattrMask::ALL,
            },
            ResultValue::Attributes(FileAttributes::default()),
        ),
        (
            Op::Setattr {
                object,
                attributes: SetAttributes::default(),
            },
            ResultValue::AttributesSet,
        ),
        (Op::Readlink { object }, ResultValue::LinkTarget("t".into())),
        (
            Op::Rename {
                object,
                directory: object,
                name: "n".into(),
            },
            ResultValue::Renamed,
        ),
        (
            Op::RenameAt {
                old_directory: object,
                old_name: "a".into(),
                new_directory: object,
                new_name: "b".into(),
            },
            ResultValue::RenamedAt,
        ),
        (
            Op::Remove {
                object,
                open: None,
                xattr: None,
            },
            ResultValue::Removed,
        ),
        (
            Op::UnlinkAt {
                directory: object,
                name: "a".into(),
                flags: UnlinkFlags::EMPTY,
            },
            ResultValue::Unlinked,
        ),
        (
            Op::Link {
                directory: object,
                target: object,
                name: "a".into(),
            },
            ResultValue::Linked,
        ),
        (
            Op::XattrWalk {
                object,
                name: "user.a".into(),
            },
            ResultValue::XattrWalked(XattrWalkResult { xattr, size: 1 }),
        ),
        (
            Op::XattrCreate {
                object,
                name: "user.a".into(),
                size: 1,
                flags: XattrFlags::EMPTY,
            },
            ResultValue::XattrCreated(XattrCreateResult { xattr }),
        ),
        (
            Op::XattrRead {
                xattr,
                offset: 0,
                count: 1,
            },
            ResultValue::XattrRead(vec![1]),
        ),
        (
            Op::XattrWrite {
                xattr,
                offset: 0,
                data: vec![1],
            },
            ResultValue::XattrWritten(1),
        ),
        (
            Op::XattrCommit {
                xattr,
                expected_size: 1,
            },
            ResultValue::XattrCommitted,
        ),
        (
            Op::Lock {
                open,
                lock: lock_request,
            },
            ResultValue::Locked(LockStatus::Success),
        ),
        (
            Op::Getlock {
                open,
                lock: lock.clone(),
            },
            ResultValue::LockQueried(lock),
        ),
    ];

    for (operation, result) in pairs {
        assert_eq!(operation.expected_result(), result.kind(), "{operation:?}");
        assert_eq!(CapabilitySet::ALL.require_for(&operation), Ok(()));
        if !matches!(operation, Op::Release { .. }) {
            assert!(CapabilitySet::NONE.require_for(&operation).is_err());
        }
        let request = w9pt::filesystem::FilesystemRequest::new(context.clone(), operation);
        assert_eq!(request.expected_result(), result.kind());
    }
}
