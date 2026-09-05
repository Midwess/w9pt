#![allow(missing_docs)]

mod common;

use aws_smithy_types::body::SdkBody;
use http_1x::Response;
use w9pt_fs_storage::{ObjectKey, ObjectRange, TargetStore};
use w9pt_fs_storage_s3::S3Error;

use common::{ok_head, replay_target, response};

fn key() -> ObjectKey {
    ObjectKey::new("private/v1/data/read-test").unwrap()
}

#[tokio::test]
async fn oversized_head_stops_before_get_body_dispatch() {
    let (target, http) = replay_target(vec![ok_head(9, "\"etag\"")], |builder| builder);
    assert!(matches!(
        target.get(key(), 8).await,
        Err(S3Error::Limit { .. })
    ));
    assert_eq!(http.actual_requests().count(), 1);
}

#[tokio::test]
async fn forbidden_head_is_not_reported_as_absence() {
    let (target, _) = replay_target(
        vec![response(
            403,
            &[("x-amz-request-id", "denied-request")],
            "<Error><Code>AccessDenied</Code></Error>",
        )],
        |builder| builder,
    );
    assert!(matches!(
        target.get(key(), 8).await,
        Err(S3Error::Service {
            status: Some(403),
            ..
        })
    ));
}

#[tokio::test]
async fn modeled_missing_head_is_none_and_range_416_is_not_absence() {
    let (missing, _) = replay_target(
        vec![response(
            404,
            &[("x-amz-request-id", "missing-request")],
            "<Error><Code>NotFound</Code></Error>",
        )],
        |builder| builder,
    );
    assert_eq!(missing.get(key(), 8).await.unwrap(), None);

    let (unsatisfied, _) = replay_target(
        vec![response(
            416,
            &[("x-amz-request-id", "range-request")],
            "<Error><Code>InvalidRange</Code></Error>",
        )],
        |builder| builder,
    );
    assert!(matches!(
        unsatisfied
            .get_range(key(), ObjectRange::new(9, 10).unwrap())
            .await,
        Err(S3Error::Service {
            status: Some(416),
            ..
        })
    ));
}

#[tokio::test]
async fn missing_bucket_is_not_reported_as_object_absence() {
    let (target, _) = replay_target(
        vec![response(
            404,
            &[("x-amz-request-id", "missing-bucket")],
            "<Error><Code>NoSuchBucket</Code></Error>",
        )],
        |builder| builder,
    );
    assert!(matches!(
        target.get(key(), 8).await,
        Err(S3Error::Service {
            status: Some(404),
            ..
        })
    ));
}

#[tokio::test]
async fn complete_read_version_races_stop_at_configured_bound() {
    let precondition = || {
        response(
            412,
            &[("x-amz-request-id", "race-request")],
            "<Error><Code>PreconditionFailed</Code></Error>",
        )
    };
    let (target, http) = replay_target(
        vec![
            ok_head(3, "\"one\""),
            precondition(),
            ok_head(3, "\"two\""),
            precondition(),
        ],
        |builder| builder.max_read_retries(1),
    );
    assert!(matches!(
        target.get(key(), 3).await,
        Err(S3Error::ReadRaceExhausted { attempts: 2 })
    ));
    assert_eq!(http.actual_requests().count(), 4);
}

#[tokio::test]
async fn empty_ranges_at_zero_and_eof_are_exact_metadata_reads() {
    let (target, http) = replay_target(
        vec![ok_head(3, "\"etag\""), ok_head(3, "\"etag\"")],
        |builder| builder,
    );
    assert_eq!(
        target
            .get_range(key(), ObjectRange::new(0, 0).unwrap())
            .await
            .unwrap(),
        Some(Vec::new())
    );
    assert_eq!(
        target
            .get_range(key(), ObjectRange::new(3, 3).unwrap())
            .await
            .unwrap(),
        Some(Vec::new())
    );
    assert_eq!(http.actual_requests().count(), 2);
}

#[tokio::test]
async fn exact_range_accepts_only_complete_partial_response() {
    let (target, _) = replay_target(
        vec![response(
            206,
            &[
                ("content-length", "3"),
                ("content-range", "bytes 1-3/6"),
                ("etag", "\"etag\""),
            ],
            "bcd",
        )],
        |builder| builder,
    );
    assert_eq!(
        target
            .get_range(key(), ObjectRange::new(1, 4).unwrap())
            .await
            .unwrap(),
        Some(b"bcd".to_vec())
    );
}

#[tokio::test]
async fn ignored_clamped_short_and_long_ranges_never_return_partial_success() {
    let cases = [
        response(
            200,
            &[
                ("content-length", "3"),
                ("content-range", "bytes 1-3/6"),
                ("etag", "\"etag\""),
            ],
            "bcd",
        ),
        response(
            206,
            &[
                ("content-length", "2"),
                ("content-range", "bytes 1-2/3"),
                ("etag", "\"etag\""),
            ],
            "bc",
        ),
        Response::builder()
            .status(206)
            .header("content-length", "3")
            .header("content-range", "bytes 1-3/6")
            .header("etag", "\"etag\"")
            .body(SdkBody::from("bc"))
            .unwrap(),
        response(
            206,
            &[
                ("content-length", "3"),
                ("content-range", "bytes 1-3/6"),
                ("etag", "\"etag\""),
            ],
            "bcde",
        ),
    ];
    for response in cases {
        let (target, _) = replay_target(vec![response], |builder| builder);
        assert!(
            target
                .get_range(key(), ObjectRange::new(1, 4).unwrap())
                .await
                .is_err()
        );
    }
}
