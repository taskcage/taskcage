use std::fmt::Debug;
use std::path::PathBuf;

use crate::application::ProfileSubmission;
use crate::execution_plan::ResolvedExecutionPlan;
use crate::resource_budget::ResourceBudget;

use super::registry::{ProfileError, ResolvedProfile};

/// 확장 가능한 Capsule resolve 경계다.
///
/// Resolver가 요청 identity를 소유하지 않을 때만 `NotFound`를 반환한다. 소유한 identity가 손상되거나
/// 사용할 수 없으면 다음 resolver로 넘기지 않고 fail-closed한다.
pub(crate) trait CapsuleResolver: Debug + Send + Sync {
    fn resolve(&self, request: &ProfileSubmission) -> Result<CapsuleResolution, ProfileError>;
}

pub(crate) enum CapsuleResolution {
    Resolved(Box<ResolvedProfile>),
    NotFound,
}

/// Resolver adapter가 고정한 executable을 staging 이후 실행 plan으로 바꾸는 port다.
pub(crate) trait ProfileExecution: Debug + Send {
    fn into_plan(
        self: Box<Self>,
        profile_name: &str,
        input: PathBuf,
        output: PathBuf,
        working_directory: PathBuf,
        budget: ResourceBudget,
    ) -> ResolvedExecutionPlan;

    #[cfg(test)]
    fn kind(&self) -> ProfileExecutionKind;
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProfileExecutionKind {
    Bundle,
    FileCopy,
    LegacyFfmpeg,
}
