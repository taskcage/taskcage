//! Task 취소 use case다. 취소 접수가 아니라 cleanup-confirmed 결과를 반환한다.

use taskcage_core::task::TaskSnapshot;

use super::ports::{RegistryError, TaskCancellationPort};

pub(crate) async fn cancel_task<P>(
    registry: &P,
    task_id: &str,
) -> Result<TaskSnapshot, RegistryError>
where
    P: TaskCancellationPort,
{
    registry.cancel_and_wait(task_id).await
}
