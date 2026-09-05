//! AWS endpoint observation used by live qualification.

use std::sync::{Arc, Mutex};

use aws_sdk_s3::{
    config::{
        ConfigBag, Intercept, RuntimeComponents, interceptors::BeforeTransmitInterceptorContextRef,
    },
    error::BoxError,
};

use crate::S3Error;

#[derive(Clone, Debug, Default)]
pub(crate) struct RequestUri(Arc<Mutex<Option<String>>>);

impl RequestUri {
    pub(crate) fn get(&self) -> Option<String> {
        self.0.lock().ok().and_then(|uri| uri.clone())
    }
}

impl Intercept for RequestUri {
    fn name(&self) -> &'static str {
        "w9pt-s3-qualification-endpoint"
    }

    fn read_before_attempt(
        &self,
        context: &BeforeTransmitInterceptorContextRef<'_>,
        _runtime_components: &RuntimeComponents,
        _config: &mut ConfigBag,
    ) -> Result<(), BoxError> {
        if let Ok(mut uri) = self.0.lock() {
            *uri = Some(context.request().uri().to_owned());
        }
        Ok(())
    }
}

pub(crate) fn validate_aws_general_purpose_uri(uri: &str, bucket: &str) -> Result<(), S3Error> {
    let rest = uri.strip_prefix("https://").ok_or(S3Error::Qualification {
        reason: "AWS qualification requires verified HTTPS",
    })?;
    let (authority, path_and_query) = rest.split_once('/').unwrap_or((rest, ""));
    if authority.is_empty() || authority.contains(['@', ':']) {
        return Err(S3Error::Qualification {
            reason: "AWS qualification observed an invalid endpoint authority",
        });
    }
    let aws_host = authority.ends_with(".amazonaws.com")
        || authority.ends_with(".amazonaws.com.cn")
        || authority == "s3.amazonaws.com";
    if !aws_host {
        return Err(S3Error::Qualification {
            reason: "client endpoint is not Amazon S3",
        });
    }

    let virtual_prefix = format!("{bucket}.");
    let virtual_hosted = authority
        .strip_prefix(&virtual_prefix)
        .is_some_and(is_s3_service_host);
    let path_style = is_s3_service_host(authority)
        && path_and_query
            .split('?')
            .next()
            .is_some_and(|path| path == bucket || path.starts_with(&format!("{bucket}/")));
    if !virtual_hosted && !path_style {
        return Err(S3Error::Qualification {
            reason: "AWS endpoint does not address the configured bucket",
        });
    }
    Ok(())
}

fn is_s3_service_host(host: &str) -> bool {
    (host == "s3.amazonaws.com"
        || host.starts_with("s3.")
        || host.starts_with("s3-")
        || host.starts_with("s3.dualstack."))
        && (host.ends_with(".amazonaws.com") || host.ends_with(".amazonaws.com.cn"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualification_accepts_aws_bucket_routes_and_rejects_custom_or_insecure_endpoints() {
        for uri in [
            "https://w9pt-test-bucket.s3.us-east-1.amazonaws.com/key",
            "https://s3.us-east-1.amazonaws.com/w9pt-test-bucket/key",
            "https://w9pt-test-bucket.s3.amazonaws.com/key",
            "https://w9pt-test-bucket.s3.cn-north-1.amazonaws.com.cn/key",
        ] {
            validate_aws_general_purpose_uri(uri, "w9pt-test-bucket").unwrap();
        }
        for uri in [
            "http://w9pt-test-bucket.s3.us-east-1.amazonaws.com/key",
            "https://127.0.0.1:8333/w9pt-test-bucket/key",
            "https://other-bucket.s3.us-east-1.amazonaws.com/key",
            "https://example.com/w9pt-test-bucket/key",
        ] {
            assert!(
                validate_aws_general_purpose_uri(uri, "w9pt-test-bucket").is_err(),
                "{uri}"
            );
        }
    }
}
