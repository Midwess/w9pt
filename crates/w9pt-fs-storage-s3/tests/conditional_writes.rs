#![allow(missing_docs)]

mod common;

use w9pt_fs_storage::{ObjectKey, PutIfAbsent, TargetStore};
use w9pt_fs_storage_s3::S3Error;

use common::{ok_head, replay_target, response};

fn key() -> ObjectKey {
    ObjectKey::new("private/v1/data/write test+punctuation").unwrap()
}

#[tokio::test]
async fn immutable_put_serializes_exact_condition_routing_checksum_and_body_once() {
    let (target, http) = replay_target(
        vec![response(200, &[("etag", "\"created\"")], "")],
        |builder| {
            builder
                .expected_bucket_owner("012345678901")
                .requester_pays(true)
        },
    );
    assert!(matches!(
        target
            .put_if_absent(key(), b"payload".to_vec())
            .await
            .unwrap(),
        PutIfAbsent::Created { .. }
    ));

    let requests = http.actual_requests().collect::<Vec<_>>();
    assert_eq!(requests.len(), 1);
    let request = requests[0];
    assert_eq!(request.method(), "PUT");
    assert_eq!(
        request.uri(),
        "https://w9pt-test-bucket.s3.us-east-1.amazonaws.com/private/v1/data/write%20test%2Bpunctuation?x-id=PutObject"
    );
    assert_eq!(request.headers().get("if-none-match"), Some("*"));
    assert_eq!(
        request.headers().get("x-amz-expected-bucket-owner"),
        Some("012345678901")
    );
    assert_eq!(
        request.headers().get("x-amz-request-payer"),
        Some("requester")
    );
    assert_eq!(
        request.headers().get("x-amz-sdk-checksum-algorithm"),
        Some("CRC32C")
    );
    assert!(request.headers().get("x-amz-checksum-crc32c").is_some());
    assert_eq!(request.headers().get("content-length"), Some("7"));
    assert_eq!(request.body().bytes(), Some(b"payload".as_slice()));
}

#[tokio::test]
async fn immutable_precondition_reads_current_without_second_mutation() {
    let (target, http) = replay_target(
        vec![
            response(
                412,
                &[("x-amz-request-id", "precondition")],
                "<Error><Code>PreconditionFailed</Code></Error>",
            ),
            ok_head(3, "\"current\""),
        ],
        |builder| builder,
    );
    assert!(matches!(
        target.put_if_absent(key(), b"new".to_vec()).await.unwrap(),
        PutIfAbsent::AlreadyExists { .. }
    ));
    let requests = http.actual_requests().collect::<Vec<_>>();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method(), "PUT");
    assert_eq!(requests[1].method(), "HEAD");
}

#[tokio::test]
async fn immutable_resolution_retries_transient_head_without_repeating_put() {
    let (target, http) = replay_target(
        vec![
            response(412, &[], "<Error><Code>PreconditionFailed</Code></Error>"),
            response(503, &[], "<Error><Code>ServiceUnavailable</Code></Error>"),
            ok_head(3, "\"current\""),
        ],
        |builder| builder.max_resolution_attempts(2),
    );
    assert!(matches!(
        target.put_if_absent(key(), b"new".to_vec()).await.unwrap(),
        PutIfAbsent::AlreadyExists { .. }
    ));
    let requests = http.actual_requests().collect::<Vec<_>>();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method() == "PUT")
            .count(),
        1
    );
}

#[tokio::test]
async fn immutable_resolution_stops_at_configured_bound() {
    let (target, http) = replay_target(
        vec![
            response(
                409,
                &[],
                "<Error><Code>ConditionalRequestConflict</Code></Error>",
            ),
            response(503, &[], "<Error><Code>ServiceUnavailable</Code></Error>"),
            response(503, &[], "<Error><Code>ServiceUnavailable</Code></Error>"),
        ],
        |builder| builder.max_resolution_attempts(2),
    );
    assert!(matches!(
        target.put_if_absent(key(), b"new".to_vec()).await,
        Err(S3Error::CurrentStateResolutionExhausted { attempts: 2, .. })
    ));
    assert_eq!(http.actual_requests().count(), 3);
}

#[tokio::test]
async fn immutable_server_error_is_ambiguous_after_one_mutation_dispatch() {
    let (target, http) = replay_target(
        vec![response(
            503,
            &[("x-amz-request-id", "unavailable")],
            "<Error><Code>ServiceUnavailable</Code></Error>",
        )],
        |builder| builder,
    );
    assert_eq!(
        target.put_if_absent(key(), b"new".to_vec()).await.unwrap(),
        PutIfAbsent::Ambiguous
    );
    assert_eq!(http.actual_requests().count(), 1);
}

#[tokio::test]
async fn immutable_success_without_etag_is_ambiguous() {
    let (target, http) = replay_target(vec![response(200, &[], "")], |builder| builder);
    assert_eq!(
        target.put_if_absent(key(), b"new".to_vec()).await.unwrap(),
        PutIfAbsent::Ambiguous
    );
    assert_eq!(http.actual_requests().count(), 1);
}

#[tokio::test]
async fn immutable_put_404_is_a_definite_service_failure() {
    let (target, http) = replay_target(
        vec![response(
            404,
            &[("x-amz-request-id", "missing-bucket")],
            "<Error><Code>NoSuchBucket</Code></Error>",
        )],
        |builder| builder,
    );
    assert!(matches!(
        target.put_if_absent(key(), b"new".to_vec()).await,
        Err(S3Error::Service {
            status: Some(404),
            ..
        })
    ));
    assert_eq!(http.actual_requests().count(), 1);
}
