#![allow(missing_docs)]

use w9pt::{
    DecodeError, Limits, LinuxErrno, Qid, Tag,
    protocol::{
        Fid, LinuxWireError, QidType, RequestBody, Response, ResponseBody, decode_request,
        encode_response,
    },
};

#[test]
fn decodes_independent_tversion_vector() {
    let bytes = [
        21, 0, 0, 0, 100, 0xff, 0xff, 0x00, 0x10, 0, 0, 8, 0, b'9', b'P', b'2', b'0', b'0', b'0',
        b'.', b'L',
    ];
    let request = decode_request(&bytes, &Limits::default(), 4096).unwrap();
    assert_eq!(request.tag, Tag::NOTAG);
    assert_eq!(
        request.body,
        RequestBody::Version {
            msize: 4096,
            version: "9P2000.L".into()
        }
    );
}

#[test]
fn decodes_independent_tattach_vector() {
    let bytes = [
        28, 0, 0, 0, 104, 3, 0, 9, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 4, 0, b'r', b'o', b'o', b't',
        1, 0, b'/', 0, 0, 0, 0,
    ];
    let request = decode_request(&bytes, &Limits::default(), 4096).unwrap();
    assert_eq!(
        request.body,
        RequestBody::Attach {
            fid: Fid::new(9),
            afid: Fid::NOFID,
            uname: "root".into(),
            aname: "/".into(),
            n_uname: 0,
        }
    );
}

#[test]
fn encodes_independent_rlerror_vector() {
    let response = Response {
        tag: Tag::new(0x1234),
        body: ResponseBody::Lerror(LinuxWireError(LinuxErrno::ENOENT)),
    };
    assert_eq!(
        encode_response(&response, 4096).unwrap(),
        [11, 0, 0, 0, 7, 0x34, 0x12, 2, 0, 0, 0]
    );
}

#[test]
fn encodes_independent_rwalk_vector() {
    let response = Response {
        tag: Tag::new(5),
        body: ResponseBody::Walk {
            qids: vec![Qid::new(
                QidType::DIRECTORY,
                0x1122_3344,
                0x0102_0304_0506_0708,
            )],
        },
    };
    assert_eq!(
        encode_response(&response, 4096).unwrap(),
        [
            22, 0, 0, 0, 111, 5, 0, 1, 0, 0x80, 0x44, 0x33, 0x22, 0x11, 8, 7, 6, 5, 4, 3, 2, 1,
        ]
    );
}

#[test]
fn malformed_outer_and_nested_lengths_are_rejected() {
    let limits = Limits::default();
    assert_eq!(
        decode_request(&[6, 0, 0, 0, 108, 1], &limits, 4096),
        Err(DecodeError::FrameTooSmall { size: 6 })
    );
    assert_eq!(
        decode_request(&[9, 0, 0, 0, 108, 1, 0], &limits, 4096),
        Err(DecodeError::LengthMismatch {
            declared: 9,
            actual: 7
        })
    );

    let truncated_string = [15, 0, 0, 0, 100, 0xff, 0xff, 64, 0, 0, 0, 8, 0, b'9', b'P'];
    assert!(matches!(
        decode_request(&truncated_string, &limits, 4096),
        Err(DecodeError::Incomplete { .. })
    ));
}

#[test]
fn trailing_unknown_response_and_invalid_utf8_are_rejected() {
    let limits = Limits::default();
    let trailing = [10, 0, 0, 0, 108, 1, 0, 2, 0, 0];
    assert_eq!(
        decode_request(&trailing, &limits, 4096),
        Err(DecodeError::TrailingBytes { remaining: 1 })
    );
    let unknown = [7, 0, 0, 0, 99, 1, 0];
    assert_eq!(
        decode_request(&unknown, &limits, 4096),
        Err(DecodeError::UnknownMessageType(99))
    );
    let response = [7, 0, 0, 0, 109, 1, 0];
    assert_eq!(
        decode_request(&response, &limits, 4096),
        Err(DecodeError::UnexpectedResponse(109))
    );
    let invalid_utf8 = [
        16, 0, 0, 0, 100, 0xff, 0xff, 64, 0, 0, 0, 3, 0, 0xff, 0xff, 0xff,
    ];
    assert!(matches!(
        decode_request(&invalid_utf8, &limits, 4096),
        Err(DecodeError::InvalidUtf8 { .. })
    ));
}

#[test]
fn oversized_walk_count_is_rejected_before_allocation() {
    let limits = Limits {
        max_walk_elements: 1,
        ..Limits::default()
    };
    let frame = [17, 0, 0, 0, 110, 1, 0, 1, 0, 0, 0, 2, 0, 0, 0, 2, 0];
    assert_eq!(
        decode_request(&frame, &limits, 4096),
        Err(DecodeError::CountTooLarge {
            kind: "walk elements",
            count: 2,
            maximum: 1
        })
    );
}
