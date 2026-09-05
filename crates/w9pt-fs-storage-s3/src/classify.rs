//! Operation-sensitive AWS SDK and S3 failure classification.

use aws_sdk_s3::{
    error::{ProvideErrorMetadata, SdkError},
    operation::{
        RequestId, RequestIdExt, get_object::GetObjectError, head_object::HeadObjectError,
        put_object::PutObjectError,
    },
};

use crate::{S3Error, S3Operation, S3RequestIds};

pub(crate) enum ReadFailure {
    Missing,
    PreconditionFailed,
    Error(S3Error),
}

pub(crate) enum MutationFailure {
    PreconditionFailed,
    ConditionalConflict,
    Missing,
    Ambiguous,
    Error(S3Error),
}

pub(crate) fn classify_head_error(error: &SdkError<HeadObjectError>) -> ReadFailure {
    let service = error.as_service_error();
    if service.is_some_and(HeadObjectError::is_not_found)
        && service
            .and_then(ProvideErrorMetadata::code)
            .is_some_and(is_object_missing_code)
    {
        return ReadFailure::Missing;
    }
    classify_read_error(error, S3Operation::Head)
}

pub(crate) fn classify_get_error(
    error: &SdkError<GetObjectError>,
    operation: S3Operation,
) -> ReadFailure {
    let service = error.as_service_error();
    if service.is_some_and(GetObjectError::is_no_such_key)
        && service
            .and_then(ProvideErrorMetadata::code)
            .is_some_and(is_object_missing_code)
    {
        return ReadFailure::Missing;
    }
    classify_read_error(error, operation)
}

fn classify_read_error<E>(error: &SdkError<E>, operation: S3Operation) -> ReadFailure
where
    E: ProvideErrorMetadata,
    SdkError<E>: RequestId + RequestIdExt,
{
    let status = error
        .raw_response()
        .map(|response| response.status().as_u16());
    if status == Some(412) {
        return ReadFailure::PreconditionFailed;
    }
    if matches!(status, Some(408 | 429 | 500..=599))
        || matches!(
            error,
            SdkError::TimeoutError(_) | SdkError::DispatchFailure(_) | SdkError::ResponseError(_)
        )
    {
        return ReadFailure::Error(S3Error::TransientRead {
            operation,
            request_ids: request_ids(error),
        });
    }
    let code = error
        .as_service_error()
        .and_then(ProvideErrorMetadata::code);
    ReadFailure::Error(S3Error::service(
        operation,
        status,
        code,
        request_ids(error),
    ))
}

pub(crate) fn request_ids<T>(value: &T) -> S3RequestIds
where
    T: RequestId + RequestIdExt,
{
    S3RequestIds::new(value.request_id(), value.extended_request_id())
}

pub(crate) fn classify_put_error(
    error: &SdkError<PutObjectError>,
    operation: S3Operation,
) -> MutationFailure {
    let status = error
        .raw_response()
        .map(|response| response.status().as_u16());
    let code = error
        .as_service_error()
        .and_then(ProvideErrorMetadata::code);
    match status {
        Some(412) => return MutationFailure::PreconditionFailed,
        Some(409) => return MutationFailure::ConditionalConflict,
        Some(404) if code.is_some_and(is_object_missing_code) => {
            return MutationFailure::Missing;
        }
        Some(408 | 429 | 500..=599) => return MutationFailure::Ambiguous,
        _ => {}
    }
    if matches!(
        error,
        SdkError::TimeoutError(_) | SdkError::DispatchFailure(_) | SdkError::ResponseError(_)
    ) {
        return MutationFailure::Ambiguous;
    }
    MutationFailure::Error(S3Error::service(
        operation,
        status,
        code,
        request_ids(error),
    ))
}

fn is_object_missing_code(code: &str) -> bool {
    matches!(code, "NoSuchKey" | "NotFound")
}

#[cfg(test)]
mod tests {
    use std::io;

    use aws_sdk_s3::{
        config::http::HttpResponse,
        error::{ConnectorError, SdkError},
        operation::put_object::PutObjectError,
    };
    use aws_smithy_types::body::SdkBody;

    use super::*;

    fn test_error() -> io::Error {
        io::Error::other("sensitive transport detail")
    }

    #[test]
    fn mutation_sdk_uncertainty_is_ambiguous_but_construction_is_definite() {
        let timeout = SdkError::<PutObjectError>::timeout_error(test_error());
        assert!(matches!(
            classify_put_error(&timeout, S3Operation::PutIfAbsent),
            MutationFailure::Ambiguous
        ));

        let dispatch = SdkError::<PutObjectError>::dispatch_failure(ConnectorError::io(Box::new(
            test_error(),
        )));
        assert!(matches!(
            classify_put_error(&dispatch, S3Operation::CompareExchange),
            MutationFailure::Ambiguous
        ));

        let raw: HttpResponse = http_1x::Response::builder()
            .status(200)
            .body(SdkBody::empty())
            .unwrap()
            .try_into()
            .unwrap();
        let parse = SdkError::<PutObjectError>::response_error(test_error(), raw);
        assert!(matches!(
            classify_put_error(&parse, S3Operation::PutIfAbsent),
            MutationFailure::Ambiguous
        ));

        let construction = SdkError::<PutObjectError>::construction_failure(test_error());
        assert!(matches!(
            classify_put_error(&construction, S3Operation::PutIfAbsent),
            MutationFailure::Error(S3Error::Service { status: None, .. })
        ));
    }
}
