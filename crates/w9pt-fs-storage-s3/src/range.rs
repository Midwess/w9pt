//! Exact half-open range request and response validation.

use std::sync::{Arc, Mutex};

use aws_sdk_s3::{
    config::{
        ConfigBag, Intercept, RuntimeComponents,
        interceptors::BeforeDeserializationInterceptorContextRef,
    },
    error::BoxError,
};
use w9pt_fs_storage::ObjectRange;

use crate::{S3Error, S3RequestIds};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HttpRange {
    header: String,
    length: usize,
}

impl HttpRange {
    pub(crate) fn header(&self) -> &str {
        &self.header
    }

    pub(crate) const fn length(&self) -> usize {
        self.length
    }
}

pub(crate) fn checked_http_range(
    range: ObjectRange,
    maximum: usize,
) -> Result<Option<HttpRange>, S3Error> {
    if range.is_empty() {
        return Ok(None);
    }
    let logical_length = range
        .end()
        .checked_sub(range.start())
        .ok_or(S3Error::ExactRange {
            reason: "range end precedes start",
            request_ids: S3RequestIds::default(),
        })?;
    let length = usize::try_from(logical_length).map_err(|_| S3Error::Limit {
        resource: "range",
        actual: logical_length,
        maximum: u64::try_from(maximum).unwrap_or(u64::MAX),
    })?;
    if length > maximum {
        return Err(S3Error::Limit {
            resource: "range",
            actual: logical_length,
            maximum: u64::try_from(maximum).unwrap_or(u64::MAX),
        });
    }
    let last = range.end().checked_sub(1).ok_or(S3Error::ExactRange {
        reason: "inclusive range end underflow",
        request_ids: S3RequestIds::default(),
    })?;
    Ok(Some(HttpRange {
        header: format!("bytes={}-{}", range.start(), last),
        length,
    }))
}

pub(crate) fn validate_content_range(
    value: &str,
    requested: ObjectRange,
    ids: S3RequestIds,
) -> Result<u64, S3Error> {
    let value = value
        .strip_prefix("bytes ")
        .ok_or_else(|| exact_error("malformed Content-Range unit", ids.clone()))?;
    let (returned, total) = value
        .split_once('/')
        .ok_or_else(|| exact_error("malformed Content-Range total", ids.clone()))?;
    if total.contains('/') {
        return Err(exact_error("malformed Content-Range total", ids));
    }
    let (start, last) = returned
        .split_once('-')
        .ok_or_else(|| exact_error("malformed Content-Range endpoints", ids.clone()))?;
    if last.contains('-') {
        return Err(exact_error("malformed Content-Range endpoints", ids));
    }
    let start = parse_canonical_u64(start)
        .ok_or_else(|| exact_error("noncanonical Content-Range start", ids.clone()))?;
    let last = parse_canonical_u64(last)
        .ok_or_else(|| exact_error("noncanonical Content-Range end", ids.clone()))?;
    let total = parse_canonical_u64(total)
        .ok_or_else(|| exact_error("noncanonical Content-Range total", ids.clone()))?;
    let requested_last = requested
        .end()
        .checked_sub(1)
        .ok_or_else(|| exact_error("empty range has no Content-Range", ids.clone()))?;
    if start != requested.start() || last != requested_last {
        return Err(exact_error(
            "S3 clamped or changed requested endpoints",
            ids,
        ));
    }
    if total <= last {
        return Err(exact_error("invalid Content-Range object length", ids));
    }
    Ok(total)
}

fn parse_canonical_u64(value: &str) -> Option<u64> {
    if value.is_empty() || (value.len() > 1 && value.starts_with('0')) {
        return None;
    }
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

fn exact_error(reason: &'static str, request_ids: S3RequestIds) -> S3Error {
    S3Error::ExactRange {
        reason,
        request_ids,
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ResponseStatus(Arc<Mutex<Option<u16>>>);

impl ResponseStatus {
    pub(crate) fn get(&self) -> Option<u16> {
        self.0.lock().ok().and_then(|status| *status)
    }
}

impl Intercept for ResponseStatus {
    fn name(&self) -> &'static str {
        "w9pt-s3-response-status"
    }

    fn read_before_deserialization(
        &self,
        context: &BeforeDeserializationInterceptorContextRef<'_>,
        _runtime_components: &RuntimeComponents,
        _config: &mut ConfigBag,
    ) -> Result<(), BoxError> {
        if let Ok(mut status) = self.0.lock() {
            *status = Some(context.response().status().as_u16());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_ranges_convert_without_clamping_or_underflow() {
        assert_eq!(
            checked_http_range(ObjectRange::new(7, 7).unwrap(), 8).unwrap(),
            None
        );
        let range = checked_http_range(ObjectRange::new(7, 10).unwrap(), 8)
            .unwrap()
            .unwrap();
        assert_eq!(range.header(), "bytes=7-9");
        assert_eq!(range.length(), 3);
        assert!(checked_http_range(ObjectRange::new(0, 9).unwrap(), 8).is_err());
        let maximum = usize::try_from(u64::MAX).unwrap_or(usize::MAX);
        let edge = checked_http_range(ObjectRange::new(u64::MAX - 1, u64::MAX).unwrap(), maximum)
            .unwrap()
            .unwrap();
        assert_eq!(
            edge.header(),
            format!("bytes={}-{}", u64::MAX - 1, u64::MAX - 1)
        );
    }

    #[test]
    fn content_range_must_be_canonical_and_exact() {
        let requested = ObjectRange::new(7, 10).unwrap();
        assert_eq!(
            validate_content_range("bytes 7-9/11", requested, S3RequestIds::default()).unwrap(),
            11
        );
        for value in [
            "bytes 7-8/11",
            "bytes 7-9/9",
            "bytes 07-9/11",
            "bytes 7-9/*",
            "bytes=7-9/11",
            "bytes 7-9/11/12",
        ] {
            assert!(
                validate_content_range(value, requested, S3RequestIds::default()).is_err(),
                "{value}"
            );
        }
    }
}
