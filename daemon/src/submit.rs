//! protocol v1 submit 요청을 side effect 전에 검증된 내부 값으로 바꾼다.

use thiserror::Error;

use crate::protocol::{PROTOCOL_VERSION, Request, SubmitTaskPayload};
use crate::resource_budget::{ResourceBudget, ResourceBudgetError};

#[derive(Debug, Clone)]
pub(crate) struct ValidatedSubmit {
    payload: SubmitTaskPayload,
    budget: ResourceBudget,
}

impl ValidatedSubmit {
    pub(crate) fn try_from_request(
        request: Request,
    ) -> Result<(String, Self), SubmitValidationError> {
        let Request::SubmitTask {
            protocol_version,
            request_id,
            payload,
        } = request
        else {
            return Err(SubmitValidationError::NotSubmitTask);
        };
        if protocol_version != PROTOCOL_VERSION {
            return Err(SubmitValidationError::UnsupportedProtocolVersion(
                protocol_version,
            ));
        }
        validate_uuid("requestId", &request_id)?;
        let submit = Self::try_from_payload(payload)?;
        Ok((request_id, submit))
    }

    pub(crate) fn try_from_payload(
        payload: SubmitTaskPayload,
    ) -> Result<Self, SubmitValidationError> {
        validate_uuid("clientRequestId", &payload.client_request_id)?;
        validate_command(&payload)?;
        let budget =
            ResourceBudget::try_from_protocol(payload.limits.clone(), payload.output.clone())?;
        Ok(Self { payload, budget })
    }

    pub(crate) fn payload(&self) -> &SubmitTaskPayload {
        &self.payload
    }

    pub(crate) fn budget(&self) -> &ResourceBudget {
        &self.budget
    }
}

fn validate_uuid(field: &'static str, value: &str) -> Result<(), SubmitValidationError> {
    let bytes = value.as_bytes();
    let valid = bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        });
    if valid {
        Ok(())
    } else {
        Err(SubmitValidationError::InvalidUuid { field })
    }
}

fn validate_command(payload: &SubmitTaskPayload) -> Result<(), SubmitValidationError> {
    let command = &payload.command;
    if !command.program.starts_with('/') {
        return Err(SubmitValidationError::ProgramNotAbsolute);
    }
    if !command.working_directory.starts_with('/') {
        return Err(SubmitValidationError::WorkingDirectoryNotAbsolute);
    }
    reject_nul("command.program", &command.program)?;
    reject_nul("command.workingDirectory", &command.working_directory)?;
    for argument in &command.args {
        reject_nul("command.args", argument)?;
    }
    for (key, value) in &command.environment {
        if key.is_empty() || key.contains('=') {
            return Err(SubmitValidationError::InvalidEnvironmentKey);
        }
        reject_nul("command.environment key", key)?;
        reject_nul("command.environment value", value)?;
    }
    Ok(())
}

fn reject_nul(field: &'static str, value: &str) -> Result<(), SubmitValidationError> {
    if value.as_bytes().contains(&0) {
        Err(SubmitValidationError::NulByte { field })
    } else {
        Ok(())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum SubmitValidationError {
    #[error("submitTask 요청이 아닙니다")]
    NotSubmitTask,
    #[error("지원하지 않는 protocolVersion입니다: {0}")]
    UnsupportedProtocolVersion(u32),
    #[error("{field} 값은 UUID여야 합니다")]
    InvalidUuid { field: &'static str },
    #[error("command.program은 절대 경로여야 합니다")]
    ProgramNotAbsolute,
    #[error("command.workingDirectory는 절대 경로여야 합니다")]
    WorkingDirectoryNotAbsolute,
    #[error("환경 변수 이름은 비어 있거나 '=' 문자를 포함할 수 없습니다")]
    InvalidEnvironmentKey,
    #[error("{field} 값에 NUL 문자가 있습니다")]
    NulByte { field: &'static str },
    #[error(transparent)]
    ResourceBudget(#[from] ResourceBudgetError),
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::protocol::{CommandSpec, CpuMax, OutputLimits, ResourceLimits};

    use super::*;

    const REQUEST_ID: &str = "11111111-1111-1111-1111-111111111111";
    const CLIENT_REQUEST_ID: &str = "22222222-2222-2222-2222-222222222222";

    fn payload() -> SubmitTaskPayload {
        SubmitTaskPayload {
            client_request_id: CLIENT_REQUEST_ID.to_owned(),
            command: CommandSpec {
                program: "/usr/bin/true".to_owned(),
                args: vec!["argument".to_owned()],
                working_directory: "/tmp".to_owned(),
                environment: BTreeMap::from([("LANG".to_owned(), "C.UTF-8".to_owned())]),
            },
            limits: ResourceLimits {
                cpu_max: CpuMax {
                    quota_micros: 1,
                    period_micros: 1,
                },
                memory_max_bytes: 1,
                pids_max: 1,
                wall_time_limit_ms: 1,
            },
            output: OutputLimits {
                stdout_tail_max_bytes: 1,
                stderr_tail_max_bytes: 1,
            },
        }
    }

    fn request(protocol_version: u32, request_id: &str, payload: SubmitTaskPayload) -> Request {
        Request::SubmitTask {
            protocol_version,
            request_id: request_id.to_owned(),
            payload,
        }
    }

    #[test]
    fn fixture_is_validated_without_changing_the_typed_payload() {
        let fixture = include_str!("../../protocol-fixtures/v1/submit-task-valid.json");
        let request: Request = serde_json::from_str(fixture).unwrap();
        let expected = match &request {
            Request::SubmitTask { payload, .. } => payload.clone(),
            _ => unreachable!(),
        };

        let (_, validated) = ValidatedSubmit::try_from_request(request).unwrap();

        assert_eq!(validated.payload(), &expected);
    }

    #[test]
    fn protocol_version_is_rejected_before_a_submit_can_be_reserved() {
        assert_eq!(
            ValidatedSubmit::try_from_request(request(2, REQUEST_ID, payload())).unwrap_err(),
            SubmitValidationError::UnsupportedProtocolVersion(2)
        );
    }

    #[test]
    fn validates_identifiers_paths_environment_and_nul_bytes() {
        let mut cases = Vec::new();

        let mut invalid_client = payload();
        invalid_client.client_request_id = "not-a-uuid".to_owned();
        cases.push((
            invalid_client,
            SubmitValidationError::InvalidUuid {
                field: "clientRequestId",
            },
        ));

        let mut relative_program = payload();
        relative_program.command.program = "usr/bin/true".to_owned();
        cases.push((relative_program, SubmitValidationError::ProgramNotAbsolute));

        let mut relative_directory = payload();
        relative_directory.command.working_directory = "tmp".to_owned();
        cases.push((
            relative_directory,
            SubmitValidationError::WorkingDirectoryNotAbsolute,
        ));

        let mut invalid_environment = payload();
        invalid_environment
            .command
            .environment
            .insert("BAD=KEY".to_owned(), "value".to_owned());
        cases.push((
            invalid_environment,
            SubmitValidationError::InvalidEnvironmentKey,
        ));

        let mut nul_argument = payload();
        nul_argument.command.args = vec!["bad\0argument".to_owned()];
        cases.push((
            nul_argument,
            SubmitValidationError::NulByte {
                field: "command.args",
            },
        ));

        for (payload, expected) in cases {
            assert_eq!(
                ValidatedSubmit::try_from_payload(payload).unwrap_err(),
                expected
            );
        }
    }

    #[test]
    fn invalid_resource_budget_is_rejected_before_reservation() {
        let mut invalid = payload();
        invalid.limits.memory_max_bytes = 0;

        assert!(matches!(
            ValidatedSubmit::try_from_payload(invalid),
            Err(SubmitValidationError::ResourceBudget(
                ResourceBudgetError::Zero {
                    field: "limits.memoryMaxBytes"
                }
            ))
        ));
    }
}
