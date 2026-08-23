//! Task 조회 use case다.

use taskcage_core::task::TaskSnapshot;

use super::ports::{RegistryError, TaskQueryPort};

pub(crate) fn task_snapshot<P>(
    registry: &P,
    task_id: &str,
) -> Result<Option<TaskSnapshot>, RegistryError>
where
    P: TaskQueryPort,
{
    registry.snapshot(task_id)
}

pub(crate) fn task_snapshot_by_client_request_id<P>(
    registry: &P,
    client_request_id: &str,
) -> Result<Option<TaskSnapshot>, RegistryError>
where
    P: TaskQueryPort,
{
    registry.snapshot_by_client_request_id(client_request_id)
}

pub(crate) fn has_client_request_id<P>(
    registry: &P,
    client_request_id: &str,
) -> Result<bool, RegistryError>
where
    P: TaskQueryPort,
{
    registry.has_client_request_id(client_request_id)
}
