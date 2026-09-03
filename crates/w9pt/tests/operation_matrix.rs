#![allow(missing_docs)]

use std::collections::HashSet;

use w9pt::protocol::{MessageType, OPERATION_MATRIX};

#[test]
fn operation_matrix_is_exhaustive_and_unambiguous() {
    let mut requests = HashSet::new();
    let mut success_responses = HashSet::new();

    for operation in OPERATION_MATRIX {
        assert!(operation.request.is_request());
        assert!(!operation.response.is_request());
        assert_eq!(
            MessageType::try_from(operation.request.to_u8()),
            Ok(operation.request)
        );
        assert_eq!(
            MessageType::try_from(operation.response.to_u8()),
            Ok(operation.response)
        );
        assert!(
            requests.insert(operation.request),
            "duplicate request: {:?}",
            operation.request
        );
        assert!(
            success_responses.insert(operation.response),
            "duplicate response: {:?}",
            operation.response
        );
    }

    assert_eq!(requests.len(), 28);
    assert!(!requests.contains(&MessageType::Rlerror));
}
