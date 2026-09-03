#![allow(missing_docs)]

use w9pt::{Effect, Session, SessionConfig, SessionContext, SessionId, SessionStatus};

fn tversion(msize: u32, version: &[u8], tag: u16) -> Vec<u8> {
    let size = 7 + 4 + 2 + version.len();
    let mut frame = Vec::new();
    frame.extend_from_slice(&(size as u32).to_le_bytes());
    frame.push(100);
    frame.extend_from_slice(&tag.to_le_bytes());
    frame.extend_from_slice(&msize.to_le_bytes());
    frame.extend_from_slice(&(version.len() as u16).to_le_bytes());
    frame.extend_from_slice(version);
    frame
}

fn session() -> Session {
    Session::new(
        SessionConfig::default(),
        SessionContext::new(SessionId::new(1)),
    )
    .unwrap()
}

#[test]
fn exact_dialect_negotiates_and_clamps_msize() {
    let mut session = session();
    session
        .receive_frame(tversion(u32::MAX, b"9P2000.L", u16::MAX))
        .unwrap();
    assert_eq!(
        session.negotiated_msize(),
        Some(SessionConfig::default().limits.max_frame_size)
    );
    let Effect::SendFrame { bytes } = session.poll_effect().unwrap() else {
        panic!("expected Rversion")
    };
    assert_eq!(bytes[4], 101);
    assert_eq!(&bytes[13..], b"9P2000.L");
}

#[test]
fn unsupported_dialect_uses_unknown_behavior() {
    let mut session = session();
    session
        .receive_frame(tversion(4096, b"9P2000", u16::MAX))
        .unwrap();
    assert_eq!(session.negotiated_msize(), None);
    let Effect::SendFrame { bytes } = session.poll_effect().unwrap() else {
        panic!("expected Rversion")
    };
    assert_eq!(bytes[4], 101);
    assert_eq!(&bytes[13..], b"unknown");
}

#[test]
fn non_version_notag_is_terminal() {
    let mut session = session();
    session
        .receive_frame(tversion(4096, b"9P2000.L", u16::MAX))
        .unwrap();
    let _ = session.poll_effect();
    session
        .receive_frame(vec![9, 0, 0, 0, 108, 0xff, 0xff, 1, 0])
        .unwrap();
    assert_eq!(session.status(), SessionStatus::Closing);
    assert!(matches!(
        session.poll_effect(),
        Some(Effect::CloseSession { .. })
    ));
}

#[test]
fn too_small_msize_closes_instead_of_emitting_oversized_reply() {
    let mut session = session();
    session
        .receive_frame(tversion(20, b"9P2000.L", u16::MAX))
        .unwrap();
    assert!(matches!(
        session.poll_effect(),
        Some(Effect::CloseSession { .. })
    ));
}
