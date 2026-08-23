//! cleanup이 확인된 실행 결과를 Registry의 immutable FINISHED 결과로 확정한다.

use taskcage_core::task::TaskSnapshot;

use crate::capacity::TaskCapacityPermit;
use crate::fail_stop::{ActiveExecution, CleanupFailureReport, FailStopCoordinator};

use super::ports::CompletionPublicationPort;

pub(crate) fn require_finished(snapshot: TaskSnapshot) -> Result<TaskSnapshot, crate::Error> {
    if matches!(snapshot, TaskSnapshot::Finished { .. }) {
        Ok(snapshot)
    } else {
        Err(crate::Error::TaskLifecycle(
            "정리가 끝난 실행 결과가 FINISHED가 아닙니다".to_owned(),
        ))
    }
}

pub(crate) fn publish_finished<P>(
    publication: P,
    capacity_permit: TaskCapacityPermit,
    active: ActiveExecution,
    fail_stop: &FailStopCoordinator,
) -> TaskSnapshot
where
    P: CompletionPublicationPort,
{
    // FINISHED 저장 뒤 실행 소유권과 슬롯을 정리하고 마지막에 호출자를 깨운다.
    active.complete();
    release_capacity(capacity_permit, fail_stop);
    let finished = publication.publish_completion();
    crate::audit::log_task_finished(&crate::protocol_mapper::task_snapshot(finished.clone()));
    finished
}

pub(crate) fn release_capacity(
    capacity_permit: TaskCapacityPermit,
    fail_stop: &FailStopCoordinator,
) {
    if fail_stop.is_fail_stopping() {
        capacity_permit.retain_for_fail_stop();
    }
}

pub(crate) fn report_running_failure(fail_stop: &FailStopCoordinator, task_id: &str) {
    fail_stop.activate(CleanupFailureReport::new(
        task_id,
        "RUNNING 작업 완료",
        vec!["검증된 FINISHED 결과"],
        "RUNNING snapshot을 유지하고 daemon 종료를 시작함",
    ));
}

pub(crate) fn report_running_publication_failure(fail_stop: &FailStopCoordinator, task_id: &str) {
    fail_stop.activate(CleanupFailureReport::new(
        task_id,
        "RUNNING 저장",
        vec!["공개 RUNNING snapshot"],
        "exec 시작 뒤 작업 상태를 저장하지 못해 daemon 종료를 시작함",
    ));
}
