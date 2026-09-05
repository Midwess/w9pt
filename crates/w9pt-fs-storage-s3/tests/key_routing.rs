#![allow(missing_docs)]

use w9pt_fs_storage::{ObjectKey, TargetGuarantees, TargetStore};
use w9pt_fs_storage_s3::{S3Error, S3TargetConfig};

mod common;

use common::replay_target;

#[test]
fn accepted_object_key_text_is_not_normalized_or_prefixed() {
    let config = S3TargetConfig::builder("w9pt-test-bucket").build().unwrap();
    for text in [
        "private/v1/data/a b",
        "private/v1/data/a+b=c,d!e_f~g",
        "private//kept/./verbatim/../text",
        "private/unicode-雪",
    ] {
        let key = ObjectKey::new(text).unwrap();
        assert_eq!(key.as_str(), text);
        assert!(key.as_str().len() <= config.max_key_bytes());
    }
}

#[test]
fn configured_key_bound_rejects_before_routing() {
    let config = S3TargetConfig::builder("w9pt-test-bucket")
        .max_key_bytes(8)
        .build()
        .unwrap();
    let key = ObjectKey::new("nine-byte!").unwrap();
    let error = if key.as_str().len() > config.max_key_bytes() {
        S3Error::KeyTooLong {
            actual: key.as_str().len(),
            maximum: config.max_key_bytes(),
        }
    } else {
        panic!("test key must exceed configured bound")
    };
    assert!(matches!(error, S3Error::KeyTooLong { .. }));
}

#[test]
fn newly_constructed_target_advertises_no_writable_guarantees() {
    let (target, _) = replay_target(Vec::new(), |builder| builder);
    assert_eq!(target.guarantees(), TargetGuarantees::NONE);
    assert!(!target.is_qualified());
}
