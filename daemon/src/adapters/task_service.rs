//! Task application ports를 concrete registry와 Linux executor로 조립한다.

use std::future::Future;
use std::sync::Arc;
use std::time::Instant;

use taskcage_core::task::TaskSnapshot;

use crate::adapters::linux_executor::TaskRunner;
pub(crate) use crate::adapters::task_registry::TaskRegistrySettings;
use crate::adapters::task_registry::{MonotonicClock, TaskRegistry};
use crate::application::UseCaseErrorCode;
use crate::application::task::ports::{
    RegistryError, RunnerPermit, SubmitFailure, TaskExecutionPort, TaskRunConfig, TaskRunFailure,
    TaskRunFailureKind, TaskUseCases,
};
use crate::application::task::submit::{
    ExecutionCompletion, ExecutionFailure, SubmissionRuntime, SubmitContext, SubmitError,
    SubmitMetadata, SubmitOutcome, ValidatedSubmit, coordinate_validated_submit_with_runtime,
};
use crate::application::task::{cancel, query};
use crate::artifact::StagedArtifactTask;
use crate::capacity::{TaskCapacity, TaskCapacitySettings};
use crate::fail_stop::FailStopCoordinator;
use crate::metrics::RuntimeMetrics;
use crate::preflight::VerifiedEnvironment;
use crate::profile::ProfileTaskRecord;

/// Inbound adapter가 사용하는 Task use case 조립 객체다.
#[derive(Debug)]
pub(crate) struct TaskService {
    pub(crate) registry: TaskRegistry<MonotonicClock>,
    pub(crate) runner: Arc<dyn TaskExecutionPort>,
    pub(crate) capacity: Arc<TaskCapacity>,
    fail_stop: Arc<FailStopCoordinator>,
    metrics: Arc<RuntimeMetrics>,
}

impl TaskService {
    pub(crate) fn initialize(
        environment: VerifiedEnvironment,
        capacity_settings: TaskCapacitySettings,
        registry_settings: TaskRegistrySettings,
        fail_stop: Arc<FailStopCoordinator>,
    ) -> crate::Result<Self> {
        Ok(Self {
            registry: TaskRegistry::initialize(registry_settings),
            runner: Arc::new(TaskRunner::initialize(environment, Arc::clone(&fail_stop))?),
            capacity: Arc::new(TaskCapacity::new(capacity_settings)),
            fail_stop,
            metrics: Arc::new(RuntimeMetrics::default()),
        })
    }

    #[cfg(test)]
    pub(crate) async fn submit<F>(
        &self,
        request: crate::protocol::Request,
        metadata: SubmitMetadata,
        finished_time: F,
    ) -> Result<SubmitOutcome, SubmitError>
    where
        F: FnOnce() -> (String, Instant) + Send + 'static,
    {
        let (request_id, validated) =
            crate::adapters::inbound::local_uds::mapper::validated_submit(request)?;
        self.submit_validated(request_id, validated, metadata, finished_time)
            .await
    }

    #[cfg(test)]
    pub(crate) fn initialize_with_cgroup_create_faults(
        environment: VerifiedEnvironment,
        capacity_settings: TaskCapacitySettings,
        registry_settings: TaskRegistrySettings,
        fail_stop: Arc<FailStopCoordinator>,
        faults: Arc<crate::cgroup::CgroupCreateFaults>,
    ) -> crate::Result<Self> {
        Ok(Self {
            registry: TaskRegistry::initialize(registry_settings),
            runner: Arc::new(TaskRunner::initialize_with_cgroup_create_faults(
                environment,
                Arc::clone(&fail_stop),
                faults,
            )?),
            capacity: Arc::new(TaskCapacity::new(capacity_settings)),
            fail_stop,
            metrics: Arc::new(RuntimeMetrics::default()),
        })
    }

    #[cfg(test)]
    pub(crate) fn initialize_with_cleanup_faults(
        environment: VerifiedEnvironment,
        capacity_settings: TaskCapacitySettings,
        registry_settings: TaskRegistrySettings,
        fail_stop: Arc<FailStopCoordinator>,
        faults: Arc<crate::cleanup_fault::CleanupFaults>,
    ) -> crate::Result<Self> {
        Ok(Self {
            registry: TaskRegistry::initialize(registry_settings),
            runner: Arc::new(TaskRunner::initialize_with_cleanup_faults(
                environment,
                Arc::clone(&fail_stop),
                faults,
            )?),
            capacity: Arc::new(TaskCapacity::new(capacity_settings)),
            fail_stop,
            metrics: Arc::new(RuntimeMetrics::default()),
        })
    }

    pub(crate) async fn submit_validated<F>(
        &self,
        request_id: String,
        validated: ValidatedSubmit,
        metadata: SubmitMetadata,
        finished_time: F,
    ) -> Result<SubmitOutcome, SubmitError>
    where
        F: FnOnce() -> (String, Instant) + Send + 'static,
    {
        let runner = Arc::clone(&self.runner);
        coordinate_validated_submit_with_runtime(
            self.registry.clone(),
            self.submission_runtime(),
            request_id,
            validated,
            metadata,
            move |config, running_sender, cancellation| async move {
                runner
                    .execute_task(
                        RunnerPermit::new(),
                        TaskRunConfig {
                            task_id: config.task_id,
                            submitted_at: config.submitted_at,
                            start_time: config.start_time,
                            cleanup_timeout: config.cleanup_timeout,
                            plan: config.plan,
                        },
                        running_sender,
                        cancellation,
                        Box::new(finished_time),
                    )
                    .await
                    .map(|completed| ExecutionCompletion::Real(completed.into_payload()))
                    .map_err(runner_execution_failure)
            },
        )
        .await
    }

    pub(crate) async fn submit_profile_validated<F>(
        &self,
        request_id: String,
        validated: ValidatedSubmit,
        metadata: SubmitMetadata,
        finished_time: F,
        profile_task: Arc<ProfileTaskRecord>,
        staged_artifacts: StagedArtifactTask,
    ) -> Result<SubmitOutcome, SubmitError>
    where
        F: FnOnce() -> (String, Instant) + Send + 'static,
    {
        let runner = Arc::clone(&self.runner);
        coordinate_validated_submit_with_runtime(
            self.registry.clone(),
            self.submission_runtime(),
            request_id,
            validated,
            metadata,
            move |config, running_sender, cancellation| async move {
                let completed = runner
                    .execute_task(
                        RunnerPermit::new(),
                        TaskRunConfig {
                            task_id: config.task_id,
                            submitted_at: config.submitted_at,
                            start_time: config.start_time,
                            cleanup_timeout: config.cleanup_timeout,
                            plan: config.plan,
                        },
                        running_sender,
                        cancellation,
                        Box::new(finished_time),
                    )
                    .await
                    .map_err(runner_execution_failure)?;
                let payload = completed.into_payload();
                profile_task
                    .finalize(&payload, staged_artifacts)
                    .map_err(|error| {
                        ExecutionFailure::new(
                            SubmitFailure::new(UseCaseErrorCode::InternalError, error.to_string()),
                            false,
                            false,
                        )
                    })?;
                Ok(ExecutionCompletion::Real(payload))
            },
        )
        .await
    }

    pub(crate) async fn cancel(&self, task_id: &str) -> Result<TaskSnapshot, RegistryError> {
        cancel::cancel_task(&self.registry, task_id).await
    }

    pub(crate) async fn wait_idle(&self) {
        self.capacity.wait_idle().await;
    }

    pub(crate) fn render_metrics(&self) -> String {
        self.metrics.render(
            self.capacity.in_use(),
            self.capacity.settings().max_concurrent_tasks(),
        )
    }

    fn submission_runtime(&self) -> SubmissionRuntime {
        SubmissionRuntime::new(
            Arc::clone(&self.capacity),
            Arc::clone(&self.fail_stop),
            Arc::clone(&self.metrics),
        )
    }

    pub(crate) fn snapshot(&self, task_id: &str) -> Result<Option<TaskSnapshot>, RegistryError> {
        query::task_snapshot(&self.registry, task_id)
    }

    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "diagnostic profile boundary keeps this test-only lookup"
        )
    )]
    pub(crate) fn snapshot_by_client_request_id(
        &self,
        client_request_id: &str,
    ) -> Result<Option<TaskSnapshot>, RegistryError> {
        query::task_snapshot_by_client_request_id(&self.registry, client_request_id)
    }

    pub(crate) fn has_client_request_id(
        &self,
        client_request_id: &str,
    ) -> Result<bool, RegistryError> {
        query::has_client_request_id(&self.registry, client_request_id)
    }

    #[cfg(test)]
    pub(crate) fn capacity_is_available_for_test(&self) -> bool {
        self.capacity.try_acquire().is_some()
    }

    #[cfg(test)]
    pub(crate) fn retained_capacity_for_test(&self) -> u32 {
        self.capacity.retained_for_fail_stop()
    }
}

impl TaskUseCases for TaskService {
    fn submit_validated(
        &self,
        request_id: String,
        validated: ValidatedSubmit,
        context: SubmitContext,
    ) -> impl Future<Output = Result<SubmitOutcome, SubmitError>> + Send {
        let (metadata, finished_time) = context.into_parts();
        TaskService::submit_validated(self, request_id, validated, metadata, finished_time)
    }

    fn snapshot(&self, task_id: &str) -> Result<Option<TaskSnapshot>, RegistryError> {
        TaskService::snapshot(self, task_id)
    }

    fn cancel(
        &self,
        task_id: &str,
    ) -> impl Future<Output = Result<TaskSnapshot, RegistryError>> + Send {
        TaskService::cancel(self, task_id)
    }
}

fn runner_execution_failure(error: TaskRunFailure) -> ExecutionFailure {
    let capacity_reusable = error.capacity_reusable();
    let cleanup_complete = error.cleanup_complete();
    let failure = match error.kind() {
        TaskRunFailureKind::CgroupReadBackMismatch => SubmitFailure::new(
            UseCaseErrorCode::InternalError,
            "cgroup limit read-back verification failed",
        ),
        TaskRunFailureKind::CgroupReadBackRollbackUncertain => SubmitFailure::new(
            UseCaseErrorCode::EnvironmentUnavailable,
            "cgroup v2 execution environment is unavailable",
        ),
        TaskRunFailureKind::Other => {
            SubmitFailure::new(UseCaseErrorCode::InternalError, error.into_message())
        }
    };
    ExecutionFailure::new(failure, capacity_reusable, cleanup_complete)
}
