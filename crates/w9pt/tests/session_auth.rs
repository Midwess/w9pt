#![allow(missing_docs)]

use w9pt::{
    AuthHandle, Completion, Effect, PolicyResult, Qid, Session, SessionConfig, SessionContext,
    SessionId,
    filesystem::{AttachResult, CapabilitySet, ExportId, ObjectHandle, PrincipalId},
    protocol::QidType,
};

fn session() -> Session {
    Session::new(
        SessionConfig::default(),
        SessionContext::new(SessionId::new(7)),
    )
    .unwrap()
}

fn negotiate(session: &mut Session) {
    session
        .receive_frame(vec![
            21, 0, 0, 0, 100, 0xff, 0xff, 0, 0x10, 0, 0, 8, 0, b'9', b'P', b'2', b'0', b'0', b'0',
            b'.', b'L',
        ])
        .unwrap();
    let _ = session.poll_effect();
}

#[test]
fn auth_and_attach_install_host_results_only_on_completion() {
    let mut session = session();
    negotiate(&mut session);

    session
        .receive_frame(vec![
            21, 0, 0, 0, 102, 1, 0, 9, 0, 0, 0, 1, 0, b'u', 1, 0, b'/', 42, 0, 0, 0,
        ])
        .unwrap();
    assert_eq!(session.fid_count(), 1);
    let Effect::Policy { operation_id, .. } = session.poll_effect().unwrap() else {
        panic!("expected auth policy effect")
    };
    let auth_qid = Qid::new(QidType::AUTH, 0, 90);
    session
        .complete(Completion::Policy {
            operation_id,
            result: Ok(PolicyResult::AuthStarted {
                qid: auth_qid,
                handle: AuthHandle::new(99),
            }),
        })
        .unwrap();
    let Effect::SendFrame { bytes } = session.poll_effect().unwrap() else {
        panic!()
    };
    assert_eq!(bytes[4], 103);

    session
        .receive_frame(vec![
            25, 0, 0, 0, 104, 2, 0, 10, 0, 0, 0, 9, 0, 0, 0, 1, 0, b'u', 1, 0, b'/', 42, 0, 0, 0,
        ])
        .unwrap();
    assert_eq!(session.fid_count(), 2);
    let Effect::Policy { operation_id, .. } = session.poll_effect().unwrap() else {
        panic!("expected attach policy effect")
    };
    let root_qid = Qid::new(QidType::DIRECTORY, 1, 1);
    session
        .complete(Completion::Policy {
            operation_id,
            result: Ok(PolicyResult::Attached(AttachResult {
                principal: PrincipalId::new("principal-42"),
                export: ExportId::new("root"),
                root: ObjectHandle::new(1),
                qid: root_qid,
                capabilities: CapabilitySet::ALL,
            })),
        })
        .unwrap();
    let Effect::SendFrame { bytes } = session.poll_effect().unwrap() else {
        panic!()
    };
    assert_eq!(bytes[4], 105);
    assert_eq!(session.fid_count(), 2);
}
