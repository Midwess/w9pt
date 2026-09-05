#![allow(dead_code)]

use aws_sdk_s3::{
    Client, Config,
    config::{BehaviorVersion, Credentials, Region, retry::RetryConfig},
};
use aws_smithy_http_client::test_util::{ReplayEvent, StaticReplayClient};
use aws_smithy_types::body::SdkBody;
use http_1x::{Request, Response};
use w9pt_fs_storage_s3::{S3Target, S3TargetConfig, S3TargetConfigBuilder};

pub fn response(status: u16, headers: &[(&str, &str)], body: &'static str) -> Response<SdkBody> {
    let mut response = Response::builder().status(status);
    for (name, value) in headers {
        response = response.header(*name, *value);
    }
    response.body(SdkBody::from(body)).unwrap()
}

pub fn replay_target(
    responses: Vec<Response<SdkBody>>,
    configure: impl FnOnce(S3TargetConfigBuilder) -> S3TargetConfigBuilder,
) -> (S3Target, StaticReplayClient) {
    let events = responses
        .into_iter()
        .map(|response| {
            ReplayEvent::new(
                Request::builder()
                    .uri("https://w9pt-test-bucket.s3.us-east-1.amazonaws.com/unused")
                    .body(SdkBody::empty())
                    .unwrap(),
                response,
            )
        })
        .collect();
    let http_client = StaticReplayClient::new(events);
    let client = Client::from_conf(
        Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .credentials_provider(Credentials::new(
                "test-access-key",
                "test-secret-key",
                None,
                None,
                "w9pt-tests",
            ))
            .region(Region::new("us-east-1"))
            .http_client(http_client.clone())
            .retry_config(RetryConfig::disabled())
            .build(),
    );
    let config = configure(S3TargetConfig::builder("w9pt-test-bucket"))
        .build()
        .unwrap();
    (S3Target::new(client, config).unwrap(), http_client)
}

pub fn ok_head(length: usize, etag: &'static str) -> Response<SdkBody> {
    response(
        200,
        &[
            ("content-length", &length.to_string()),
            ("etag", etag),
            ("x-amz-request-id", "head-request"),
        ],
        "",
    )
}
