use std::fmt::Debug;

use crate::protocol::ProfileRequestPayload;

use super::registry::{ProfileError, ResolvedProfile};

/// 확장 가능한 Capsule resolve 경계다.
///
/// Resolver가 요청 identity를 소유하지 않을 때만 `NotFound`를 반환한다. 소유한 identity가 손상되거나
/// 사용할 수 없으면 다음 resolver로 넘기지 않고 fail-closed한다.
pub(super) trait CapsuleResolver: Debug + Send + Sync {
    fn resolve(&self, request: &ProfileRequestPayload) -> Result<CapsuleResolution, ProfileError>;
}

pub(super) enum CapsuleResolution {
    Resolved(Box<ResolvedProfile>),
    NotFound,
}
