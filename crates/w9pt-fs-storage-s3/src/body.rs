//! Bounded S3 response-body collection.

use aws_sdk_s3::primitives::ByteStream;
use tokio::time::timeout;

use crate::{BodyTimeout, S3Error};

pub(crate) async fn collect_exact(
    mut body: ByteStream,
    expected: usize,
    maximum: usize,
    deadlines: BodyTimeout,
) -> Result<Vec<u8>, S3Error> {
    if expected > maximum {
        return Err(S3Error::Limit {
            resource: "body",
            actual: u64::try_from(expected).unwrap_or(u64::MAX),
            maximum: u64::try_from(maximum).unwrap_or(u64::MAX),
        });
    }
    let collect = async {
        let mut bytes = Vec::with_capacity(expected);
        loop {
            let next = timeout(deadlines.stall(), body.next())
                .await
                .map_err(|_| S3Error::BodyStallTimeout)?;
            let Some(chunk) = next else {
                break;
            };
            let chunk = chunk.map_err(|_| S3Error::BodyStream)?;
            let new_length = bytes
                .len()
                .checked_add(chunk.len())
                .ok_or(S3Error::BodyLength {
                    expected: u64::try_from(expected).unwrap_or(u64::MAX),
                    actual: u64::MAX,
                })?;
            if new_length > expected || new_length > maximum {
                return Err(S3Error::BodyLength {
                    expected: u64::try_from(expected).unwrap_or(u64::MAX),
                    actual: u64::try_from(new_length).unwrap_or(u64::MAX),
                });
            }
            bytes.extend_from_slice(&chunk);
        }
        if bytes.len() != expected {
            return Err(S3Error::BodyLength {
                expected: u64::try_from(expected).unwrap_or(u64::MAX),
                actual: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            });
        }
        Ok(bytes)
    };
    timeout(deadlines.total(), collect)
        .await
        .map_err(|_| S3Error::BodyTotalTimeout)?
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn deadlines() -> BodyTimeout {
        BodyTimeout::new(Duration::from_secs(1), Duration::from_millis(100)).unwrap()
    }

    #[tokio::test]
    async fn exact_empty_short_long_and_precollection_bounds_are_checked() {
        assert_eq!(
            collect_exact(ByteStream::from(Vec::new()), 0, 8, deadlines())
                .await
                .unwrap(),
            Vec::<u8>::new()
        );
        assert_eq!(
            collect_exact(ByteStream::from(vec![1, 2, 3]), 3, 8, deadlines())
                .await
                .unwrap(),
            vec![1, 2, 3]
        );
        assert!(matches!(
            collect_exact(ByteStream::from(vec![1, 2]), 3, 8, deadlines()).await,
            Err(S3Error::BodyLength {
                expected: 3,
                actual: 2
            })
        ));
        assert!(matches!(
            collect_exact(ByteStream::from(vec![1, 2, 3, 4]), 3, 8, deadlines()).await,
            Err(S3Error::BodyLength {
                expected: 3,
                actual: 4
            })
        ));
        assert!(matches!(
            collect_exact(ByteStream::from(vec![1]), 9, 8, deadlines()).await,
            Err(S3Error::Limit { .. })
        ));
    }
}
