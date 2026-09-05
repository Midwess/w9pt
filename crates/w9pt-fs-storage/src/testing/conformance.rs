//! Reusable target-contract conformance checks.

use core::fmt;

use crate::{
    CompareExchange, ConfigurationError, InvalidObjectKey, ObjectKey, ObjectRange, PutIfAbsent,
    TargetStore,
};

/// Failure returned by the reusable target conformance suite.
#[derive(Debug)]
pub enum TargetConformanceError<E> {
    /// Adapter rejected the required writable guarantees.
    Configuration(ConfigurationError),
    /// Test namespace could not be converted into a target key.
    Key(InvalidObjectKey),
    /// Target adapter returned a definitive error.
    Target(E),
    /// An observed result violated the target contract.
    Assertion(&'static str),
}

impl<E: fmt::Display> fmt::Display for TargetConformanceError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(error) => error.fmt(formatter),
            Self::Key(error) => error.fmt(formatter),
            Self::Target(error) => error.fmt(formatter),
            Self::Assertion(message) => formatter.write_str(message),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for TargetConformanceError<E> {}

/// Exercises exact reads/ranges, immutable creation, CAS, and read-after-write.
///
/// `namespace` must identify keys not used by another concurrent conformance run.
pub async fn check_target_conformance<S: TargetStore>(
    target: &S,
    namespace: &str,
) -> Result<(), TargetConformanceError<S::Error>> {
    target
        .guarantees()
        .validate_writable()
        .map_err(TargetConformanceError::Configuration)?;
    check_target_operations(target, namespace).await
}

/// Exercises target operations without first trusting advertised guarantees.
///
/// Provider adapters use this only while establishing qualification. Ordinary
/// callers should use [`check_target_conformance`].
pub async fn check_target_operations<S: TargetStore>(
    target: &S,
    namespace: &str,
) -> Result<(), TargetConformanceError<S::Error>> {
    let immutable =
        ObjectKey::new(format!("{namespace}/immutable")).map_err(TargetConformanceError::Key)?;
    let mutable =
        ObjectKey::new(format!("{namespace}/mutable")).map_err(TargetConformanceError::Key)?;

    let first_put = target
        .put_if_absent(immutable.clone(), b"abcdef".to_vec())
        .await
        .map_err(TargetConformanceError::Target)?;
    let PutIfAbsent::Created { version: created } = first_put else {
        return Err(TargetConformanceError::Assertion(
            "put-if-absent did not create a fresh key",
        ));
    };
    let second_put = target
        .put_if_absent(immutable.clone(), b"replacement".to_vec())
        .await
        .map_err(TargetConformanceError::Target)?;
    if second_put
        != (PutIfAbsent::AlreadyExists {
            version: created.clone(),
        })
    {
        return Err(TargetConformanceError::Assertion(
            "put-if-absent replaced bytes or changed the version",
        ));
    }

    let exact = target
        .get(immutable.clone(), 6)
        .await
        .map_err(TargetConformanceError::Target)?
        .ok_or(TargetConformanceError::Assertion(
            "created object was not immediately readable",
        ))?;
    if exact.bytes() != b"abcdef" || exact.version() != &created {
        return Err(TargetConformanceError::Assertion(
            "exact read did not preserve bytes and opaque version",
        ));
    }
    if target.get(immutable.clone(), 5).await.is_ok() {
        return Err(TargetConformanceError::Assertion(
            "exact read did not enforce its pre-allocation byte bound",
        ));
    }

    let range = target
        .get_range(
            immutable.clone(),
            ObjectRange::new(1, 5).expect("ordered test range"),
        )
        .await
        .map_err(TargetConformanceError::Target)?;
    if range.as_deref() != Some(b"bcde") {
        return Err(TargetConformanceError::Assertion(
            "range read was not exact",
        ));
    }
    if target
        .get_range(
            immutable,
            ObjectRange::new(0, 7).expect("ordered test range"),
        )
        .await
        .is_ok()
    {
        return Err(TargetConformanceError::Assertion(
            "out-of-bounds exact range did not fail",
        ));
    }

    let first_cas = target
        .compare_exchange(mutable.clone(), None, b"one".to_vec())
        .await
        .map_err(TargetConformanceError::Target)?;
    let CompareExchange::Replaced {
        version: first_version,
    } = first_cas
    else {
        return Err(TargetConformanceError::Assertion(
            "CAS did not create an absent key",
        ));
    };

    let conflict = target
        .compare_exchange(mutable.clone(), None, b"lost".to_vec())
        .await
        .map_err(TargetConformanceError::Target)?;
    if !matches!(
        conflict,
        CompareExchange::Conflict {
            current: Some(ref current)
        } if current == &first_version
    ) {
        return Err(TargetConformanceError::Assertion(
            "CAS conflict did not preserve the current revision",
        ));
    }

    let replacement = target
        .compare_exchange(
            mutable.clone(),
            Some(first_version.clone()),
            b"two".to_vec(),
        )
        .await
        .map_err(TargetConformanceError::Target)?;
    let CompareExchange::Replaced {
        version: second_version,
    } = replacement
    else {
        return Err(TargetConformanceError::Assertion(
            "CAS replacement failed with the matching revision",
        ));
    };
    if first_version == second_version {
        return Err(TargetConformanceError::Assertion(
            "successful CAS did not change the opaque revision",
        ));
    }

    let published = target
        .get(mutable, 3)
        .await
        .map_err(TargetConformanceError::Target)?
        .ok_or(TargetConformanceError::Assertion(
            "CAS replacement was not immediately readable",
        ))?;
    if published.bytes() != b"two" || published.version() != &second_version {
        return Err(TargetConformanceError::Assertion(
            "read-after-publication did not return replacement bytes",
        ));
    }

    Ok(())
}

/// Exercises cross-client visibility and concurrent conditional operations.
///
/// `first` and `second` must be independently held clients for the same target
/// namespace. `namespace` must not be used by another conformance run.
pub async fn check_target_pair_conformance<S: TargetStore>(
    first: &S,
    second: &S,
    namespace: &str,
) -> Result<(), TargetConformanceError<S::Error>> {
    first
        .guarantees()
        .validate_writable()
        .map_err(TargetConformanceError::Configuration)?;
    second
        .guarantees()
        .validate_writable()
        .map_err(TargetConformanceError::Configuration)?;
    check_target_pair_operations(first, second, namespace).await
}

/// Exercises cross-client operations without trusting advertised guarantees.
///
/// Provider adapters use this only while establishing qualification. Ordinary
/// callers should use [`check_target_pair_conformance`].
pub async fn check_target_pair_operations<S: TargetStore>(
    first: &S,
    second: &S,
    namespace: &str,
) -> Result<(), TargetConformanceError<S::Error>> {
    let missing =
        ObjectKey::new(format!("{namespace}/missing")).map_err(TargetConformanceError::Key)?;
    if first
        .get(missing.clone(), 1)
        .await
        .map_err(TargetConformanceError::Target)?
        .is_some()
        || first
            .get_range(
                missing,
                ObjectRange::new(0, 0).expect("ordered empty range"),
            )
            .await
            .map_err(TargetConformanceError::Target)?
            .is_some()
    {
        return Err(TargetConformanceError::Assertion(
            "missing complete or empty-range read returned an object",
        ));
    }

    let immutable = ObjectKey::new(format!("{namespace}/concurrent-immutable"))
        .map_err(TargetConformanceError::Key)?;
    let first_key = immutable.clone();
    let second_key = immutable.clone();
    let (left, right) = join2(
        first.put_if_absent(first_key, b"left-value".to_vec()),
        second.put_if_absent(second_key, b"right-value".to_vec()),
    )
    .await;
    let left = left.map_err(TargetConformanceError::Target)?;
    let right = right.map_err(TargetConformanceError::Target)?;
    let created = match (&left, &right) {
        (PutIfAbsent::Created { .. }, PutIfAbsent::AlreadyExists { .. }) => {
            b"left-value".as_slice()
        }
        (PutIfAbsent::AlreadyExists { .. }, PutIfAbsent::Created { .. }) => {
            b"right-value".as_slice()
        }
        _ => {
            return Err(TargetConformanceError::Assertion(
                "concurrent immutable writers did not produce one creator",
            ));
        }
    };
    let visible = second
        .get(immutable.clone(), created.len())
        .await
        .map_err(TargetConformanceError::Target)?
        .ok_or(TargetConformanceError::Assertion(
            "concurrent immutable result was not visible across clients",
        ))?;
    if visible.bytes() != created {
        return Err(TargetConformanceError::Assertion(
            "concurrent immutable writer replaced or mixed bytes",
        ));
    }
    if first
        .get(immutable.clone(), created.len() - 1)
        .await
        .is_ok()
    {
        return Err(TargetConformanceError::Assertion(
            "paired complete read ignored its byte bound",
        ));
    }
    for offset in [
        0,
        u64::try_from(created.len()).expect("test length fits u64"),
    ] {
        let empty = second
            .get_range(
                immutable.clone(),
                ObjectRange::new(offset, offset).expect("ordered empty range"),
            )
            .await
            .map_err(TargetConformanceError::Target)?;
        if empty != Some(Vec::new()) {
            return Err(TargetConformanceError::Assertion(
                "present empty range was not returned exactly",
            ));
        }
    }

    let mutable = ObjectKey::new(format!("{namespace}/concurrent-cas"))
        .map_err(TargetConformanceError::Key)?;
    let initial = first
        .compare_exchange(mutable.clone(), None, b"initial".to_vec())
        .await
        .map_err(TargetConformanceError::Target)?;
    let CompareExchange::Replaced { version } = initial else {
        return Err(TargetConformanceError::Assertion(
            "paired CAS could not establish initial version",
        ));
    };
    let first_key = mutable.clone();
    let second_key = mutable.clone();
    let first_version = version.clone();
    let second_version = version;
    let (left, right) = join2(
        first.compare_exchange(first_key, Some(first_version), b"left".to_vec()),
        second.compare_exchange(second_key, Some(second_version), b"right".to_vec()),
    )
    .await;
    let left = left.map_err(TargetConformanceError::Target)?;
    let right = right.map_err(TargetConformanceError::Target)?;
    if !matches!(
        (&left, &right),
        (
            CompareExchange::Replaced { .. },
            CompareExchange::Conflict { .. }
        ) | (
            CompareExchange::Conflict { .. },
            CompareExchange::Replaced { .. }
        )
    ) {
        return Err(TargetConformanceError::Assertion(
            "concurrent CAS did not produce one replacement and one conflict",
        ));
    }

    Ok(())
}

async fn join2<A, B>(left: A, right: B) -> (A::Output, B::Output)
where
    A: core::future::Future,
    B: core::future::Future,
{
    let mut left = core::pin::pin!(left);
    let mut right = core::pin::pin!(right);
    let mut left_output = None;
    let mut right_output = None;
    core::future::poll_fn(|context| {
        if left_output.is_none()
            && let core::task::Poll::Ready(output) = left.as_mut().poll(context)
        {
            left_output = Some(output);
        }
        if right_output.is_none()
            && let core::task::Poll::Ready(output) = right.as_mut().poll(context)
        {
            right_output = Some(output);
        }
        match (left_output.take(), right_output.take()) {
            (Some(left), Some(right)) => core::task::Poll::Ready((left, right)),
            (left, right) => {
                left_output = left;
                right_output = right;
                core::task::Poll::Pending
            }
        }
    })
    .await
}
