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
