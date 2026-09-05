#![allow(missing_docs)]

mod common;

use w9pt_fs_storage::{CompareExchange, ObjectKey, PutIfAbsent, TargetStore};
use w9pt_fs_storage_s3::S3Error;

use common::{ok_head, replay_target, response};

fn key() -> ObjectKey {
    ObjectKey::new("private/v1/refs/files/cas-test").unwrap()
}

#[tokio::test]
async fn transient_write_statuses_are_ambiguous_after_one_attempt() {
    for status in [408, 429, 500, 503] {
        let (target, http) = replay_target(
            vec![response(
                status,
                &[("x-amz-request-id", "ambiguous-request")],
                "<Error><Code>TransientFailure</Code><Message>secret body</Message></Error>",
            )],
            |builder| builder,
        );
        assert_eq!(
            target
                .put_if_absent(key(), b"bytes".to_vec())
                .await
                .unwrap(),
            PutIfAbsent::Ambiguous,
            "status {status}"
        );
        assert_eq!(http.actual_requests().count(), 1, "status {status}");
    }
}

#[tokio::test]
async fn definite_write_authorization_error_is_redacted_and_correlated() {
    let (target, _) = replay_target(
        vec![response(
            403,
            &[("x-amz-request-id", "denied-request")],
            "<Error><Code>AccessDenied</Code><Message>secret body</Message></Error>",
        )],
        |builder| builder,
    );
    let error = target
        .put_if_absent(key(), b"bytes".to_vec())
        .await
        .unwrap_err();
    assert!(matches!(
        &error,
        S3Error::Service {
            status: Some(403),
            ..
        }
    ));
    let diagnostic = error.to_string();
    assert!(diagnostic.contains("denied-request"));
    assert!(!diagnostic.contains("secret body"));
}

#[tokio::test]
async fn immutable_409_observes_current_state_without_repeating_put() {
    let (target, http) = replay_target(
        vec![
            response(
                409,
                &[("x-amz-request-id", "conditional-conflict")],
                "<Error><Code>ConditionalRequestConflict</Code></Error>",
            ),
            ok_head(5, "\"current\""),
        ],
        |builder| builder,
    );
    assert!(matches!(
        target
            .put_if_absent(key(), b"bytes".to_vec())
            .await
            .unwrap(),
        PutIfAbsent::AlreadyExists { .. }
    ));
    let requests = http.actual_requests().collect::<Vec<_>>();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method(), "PUT");
    assert_eq!(requests[1].method(), "HEAD");
}

#[tokio::test]
async fn immutable_conflict_retries_transient_heads_without_repeating_put() {
    let (target, http) = replay_target(
        vec![
            response(
                409,
                &[("x-amz-request-id", "conditional-conflict")],
                "<Error><Code>ConditionalRequestConflict</Code></Error>",
            ),
            response(
                503,
                &[("x-amz-request-id", "transient-head")],
                "<Error><Code>ServiceUnavailable</Code></Error>",
            ),
            ok_head(5, "\"current\""),
        ],
        |builder| builder.max_resolution_attempts(2),
    );
    assert!(matches!(
        target
            .put_if_absent(key(), b"bytes".to_vec())
            .await
            .unwrap(),
        PutIfAbsent::AlreadyExists { .. }
    ));
    let requests = http.actual_requests().collect::<Vec<_>>();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method() == "PUT")
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method() == "HEAD")
            .count(),
        2
    );
}

#[tokio::test]
async fn immutable_conflict_exhaustion_is_definite_and_bounded() {
    let (target, http) = replay_target(
        vec![
            response(
                409,
                &[("x-amz-request-id", "conditional-conflict")],
                "<Error><Code>ConditionalRequestConflict</Code></Error>",
            ),
            response(404, &[], "<Error><Code>NotFound</Code></Error>"),
            response(404, &[], "<Error><Code>NotFound</Code></Error>"),
        ],
        |builder| builder.max_resolution_attempts(2),
    );
    assert_eq!(
        target.put_if_absent(key(), b"bytes".to_vec()).await,
        Err(S3Error::CurrentStateResolutionExhausted {
            operation: w9pt_fs_storage_s3::S3Operation::PutIfAbsent,
            attempts: 2,
        })
    );
    let requests = http.actual_requests().collect::<Vec<_>>();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method() == "PUT")
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method() == "HEAD")
            .count(),
        2
    );
}

#[tokio::test]
async fn cas_uses_exact_etag_and_never_s3_version_id() {
    let (target, http) = replay_target(
        vec![
            response(
                200,
                &[
                    ("etag", "\"first-Case\""),
                    ("x-amz-version-id", "not-a-predicate"),
                ],
                "",
            ),
            response(200, &[("etag", "\"second-Case\"")], ""),
        ],
        |builder| builder,
    );
    let CompareExchange::Replaced { version } = target
        .compare_exchange(key(), None, b"first".to_vec())
        .await
        .unwrap()
    else {
        panic!("absent CAS must succeed");
    };
    assert!(matches!(
        target
            .compare_exchange(key(), Some(version), b"second".to_vec())
            .await
            .unwrap(),
        CompareExchange::Replaced { .. }
    ));
    let requests = http.actual_requests().collect::<Vec<_>>();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].headers().get("if-none-match"), Some("*"));
    assert_eq!(
        requests[1].headers().get("if-match"),
        Some("\"first-Case\"")
    );
    assert!(requests[1].headers().get("x-amz-version-id").is_none());
}

#[tokio::test]
async fn foreign_cas_version_fails_before_dispatch() {
    let (target, http) = replay_target(Vec::new(), |builder| builder);
    let error = target
        .compare_exchange(
            key(),
            Some(w9pt_fs_storage::ObjectVersion::new(vec![0, 1, 2])),
            b"bytes".to_vec(),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, S3Error::InvalidVersion { .. }));
    assert_eq!(http.actual_requests().count(), 0);
}

#[tokio::test]
async fn quoted_empty_cas_version_fails_before_dispatch() {
    let (target, http) = replay_target(Vec::new(), |builder| builder);
    let quoted_empty = w9pt_fs_storage::ObjectVersion::new(vec![b'S', 1, 0, 2, b'"', b'"']);
    assert!(matches!(
        target
            .compare_exchange(key(), Some(quoted_empty), b"bytes".to_vec())
            .await,
        Err(S3Error::InvalidVersion { .. })
    ));
    assert_eq!(http.actual_requests().count(), 0);
}

#[tokio::test]
async fn cas_412_and_present_cas_404_return_observed_conflicts() {
    let (stale, stale_http) = replay_target(
        vec![
            response(200, &[("etag", "\"base\"")], ""),
            response(412, &[], "<Error><Code>PreconditionFailed</Code></Error>"),
            ok_head(7, "\"current\""),
        ],
        |builder| builder,
    );
    let CompareExchange::Replaced { version } = stale
        .compare_exchange(key(), None, b"base".to_vec())
        .await
        .unwrap()
    else {
        panic!("setup CAS must succeed");
    };
    assert!(matches!(
        stale
            .compare_exchange(key(), Some(version), b"stale".to_vec())
            .await
            .unwrap(),
        CompareExchange::Conflict { current: Some(_) }
    ));
    assert_eq!(
        stale_http
            .actual_requests()
            .filter(|request| request.method() == "PUT")
            .count(),
        2
    );

    let (missing, _) = replay_target(
        vec![
            response(200, &[("etag", "\"base\"")], ""),
            response(404, &[], "<Error><Code>NoSuchKey</Code></Error>"),
            response(404, &[], "<Error><Code>NotFound</Code></Error>"),
        ],
        |builder| builder,
    );
    let CompareExchange::Replaced { version } = missing
        .compare_exchange(key(), None, b"base".to_vec())
        .await
        .unwrap()
    else {
        panic!("setup CAS must succeed");
    };
    assert_eq!(
        missing
            .compare_exchange(key(), Some(version), b"next".to_vec())
            .await
            .unwrap(),
        CompareExchange::Conflict { current: None }
    );
}

#[tokio::test]
async fn present_cas_missing_retries_transient_observation_then_proves_absence() {
    let (target, http) = replay_target(
        vec![
            response(200, &[("etag", "\"base\"")], ""),
            response(404, &[], "<Error><Code>NoSuchKey</Code></Error>"),
            response(
                503,
                &[("x-amz-request-id", "transient-head")],
                "<Error><Code>ServiceUnavailable</Code></Error>",
            ),
            response(404, &[], "<Error><Code>NotFound</Code></Error>"),
        ],
        |builder| builder.max_resolution_attempts(2),
    );
    let CompareExchange::Replaced { version } = target
        .compare_exchange(key(), None, b"base".to_vec())
        .await
        .unwrap()
    else {
        panic!("setup CAS must succeed");
    };
    assert_eq!(
        target
            .compare_exchange(key(), Some(version), b"next".to_vec())
            .await
            .unwrap(),
        CompareExchange::Conflict { current: None }
    );
    let requests = http.actual_requests().collect::<Vec<_>>();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method() == "PUT")
            .count(),
        2
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method() == "HEAD")
            .count(),
        2
    );
}

#[tokio::test]
async fn cas_conflict_observation_exhaustion_is_definite_and_bounded() {
    let (target, http) = replay_target(
        vec![
            response(412, &[], "<Error><Code>PreconditionFailed</Code></Error>"),
            response(503, &[], "<Error><Code>ServiceUnavailable</Code></Error>"),
            response(503, &[], "<Error><Code>ServiceUnavailable</Code></Error>"),
        ],
        |builder| builder.max_resolution_attempts(2),
    );
    assert_eq!(
        target.compare_exchange(key(), None, b"next".to_vec()).await,
        Err(S3Error::CurrentStateResolutionExhausted {
            operation: w9pt_fs_storage_s3::S3Operation::CompareExchange,
            attempts: 2,
        })
    );
    let requests = http.actual_requests().collect::<Vec<_>>();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method() == "PUT")
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method() == "HEAD")
            .count(),
        2
    );
}

#[tokio::test]
async fn present_cas_no_such_bucket_is_not_an_object_conflict() {
    let (target, http) = replay_target(
        vec![
            response(200, &[("etag", "\"base\"")], ""),
            response(404, &[], "<Error><Code>NoSuchBucket</Code></Error>"),
        ],
        |builder| builder,
    );
    let CompareExchange::Replaced { version } = target
        .compare_exchange(key(), None, b"base".to_vec())
        .await
        .unwrap()
    else {
        panic!("setup CAS must succeed");
    };
    assert!(matches!(
        target
            .compare_exchange(key(), Some(version), b"next".to_vec())
            .await,
        Err(S3Error::Service {
            status: Some(404),
            ..
        })
    ));
    assert_eq!(http.actual_requests().count(), 2);
}

#[tokio::test]
async fn cas_missing_or_malformed_success_metadata_is_ambiguous() {
    for headers in [Vec::new(), vec![("etag", "unquoted")]] {
        let (target, http) = replay_target(vec![response(200, &headers, "")], |builder| builder);
        assert_eq!(
            target
                .compare_exchange(key(), None, b"bytes".to_vec())
                .await
                .unwrap(),
            CompareExchange::Ambiguous
        );
        assert_eq!(http.actual_requests().count(), 1);
    }
}
