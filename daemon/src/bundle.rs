//! 서명된 Bundle archive와 catalog의 기존 공개 경로를 유지한다.

#[cfg(test)]
pub(crate) use crate::adapters::outbound::bundle_catalog::test_support;
pub(crate) use crate::adapters::outbound::bundle_catalog::valid_capsule_name;
pub use crate::adapters::outbound::bundle_catalog::*;
