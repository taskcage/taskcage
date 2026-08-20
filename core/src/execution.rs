//! Backend-independent command values consumed by execution adapters.
//!
//! This module deliberately does not start a process or inspect the host. The daemon and the
//! private embedded helper are responsible for applying their host policy to these values.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// An executable selected by a validated Capsule/Profile contract.
#[derive(Debug, Clone)]
pub enum ExecutionExecutable {
    /// A validated absolute path used by the compatibility/raw execution path.
    Path(OsString),
    /// A verified Runtime Package entrypoint pinned by its open descriptor.
    Pinned {
        descriptor: Arc<File>,
        argv0: OsString,
    },
}

/// A shell-free command and its process environment.
#[derive(Debug, Clone)]
pub struct ExecutionCommand {
    executable: ExecutionExecutable,
    arguments: Vec<OsString>,
    working_directory: PathBuf,
    environment: BTreeMap<OsString, OsString>,
}

impl ExecutionCommand {
    pub fn new(
        executable: ExecutionExecutable,
        arguments: Vec<OsString>,
        working_directory: PathBuf,
        environment: BTreeMap<OsString, OsString>,
    ) -> Self {
        Self {
            executable,
            arguments,
            working_directory,
            environment,
        }
    }

    pub fn into_parts(
        self,
    ) -> (
        ExecutionExecutable,
        Vec<OsString>,
        PathBuf,
        BTreeMap<OsString, OsString>,
    ) {
        (
            self.executable,
            self.arguments,
            self.working_directory,
            self.environment,
        )
    }

    pub fn working_directory(&self) -> &Path {
        &self.working_directory
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_command_tokens_without_shell_interpretation() {
        let command = ExecutionCommand::new(
            ExecutionExecutable::Path(OsString::from("/opt/task cage/bin/tool")),
            vec![
                OsString::from("value with spaces"),
                OsString::from("$HOME"),
                OsString::from("; touch /tmp/must-not-exist"),
            ],
            PathBuf::from("/srv/task cage/job"),
            BTreeMap::new(),
        );

        let (executable, arguments, working_directory, environment) = command.into_parts();
        assert!(matches!(
            executable,
            ExecutionExecutable::Path(value) if value == "/opt/task cage/bin/tool"
        ));
        assert_eq!(
            arguments,
            vec![
                OsString::from("value with spaces"),
                OsString::from("$HOME"),
                OsString::from("; touch /tmp/must-not-exist"),
            ]
        );
        assert_eq!(working_directory, PathBuf::from("/srv/task cage/job"));
        assert!(environment.is_empty());
    }
}
