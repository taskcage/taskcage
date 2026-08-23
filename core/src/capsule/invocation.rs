use super::CapsuleIdentity;

/// Exact Profile identity supplied by an application or execution adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileIdentity {
    name: String,
    version: String,
}

impl ProfileIdentity {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn version(&self) -> &str {
        &self.version
    }
}

/// Transport-neutral request for one exact Capsule/Profile invocation.
///
/// Input pairs preserve multiplicity so duplicate names can be rejected before execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapsuleInvocation {
    client_request_id: String,
    capsule: CapsuleIdentity,
    profile: ProfileIdentity,
    inputs: Vec<(String, ProfileValue)>,
    resource_overrides: Option<ProfileResourceOverrides>,
}

/// Transport-neutral request to invoke one exact Profile.
///
/// Input pairs preserve multiplicity so duplicate names can be rejected before execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileCall {
    identity: ProfileIdentity,
    inputs: Vec<(String, ProfileValue)>,
    resource_overrides: Option<ProfileResourceOverrides>,
}

impl ProfileCall {
    pub fn new<I, N>(identity: ProfileIdentity, inputs: I) -> Self
    where
        I: IntoIterator<Item = (N, ProfileValue)>,
        N: Into<String>,
    {
        Self {
            identity,
            inputs: inputs
                .into_iter()
                .map(|(name, value)| (name.into(), value))
                .collect(),
            resource_overrides: None,
        }
    }

    pub fn with_resource_overrides(mut self, overrides: ProfileResourceOverrides) -> Self {
        self.resource_overrides = Some(overrides);
        self
    }

    pub fn identity(&self) -> &ProfileIdentity {
        &self.identity
    }

    pub fn inputs(&self) -> impl ExactSizeIterator<Item = (&str, &ProfileValue)> {
        self.inputs
            .iter()
            .map(|(name, value)| (name.as_str(), value))
    }

    pub fn resource_overrides(&self) -> Option<&ProfileResourceOverrides> {
        self.resource_overrides.as_ref()
    }

    pub fn into_parts(
        self,
    ) -> (
        ProfileIdentity,
        Vec<(String, ProfileValue)>,
        Option<ProfileResourceOverrides>,
    ) {
        (self.identity, self.inputs, self.resource_overrides)
    }
}

impl CapsuleInvocation {
    pub fn new<I, N>(
        client_request_id: impl Into<String>,
        capsule: CapsuleIdentity,
        profile: ProfileIdentity,
        inputs: I,
    ) -> Self
    where
        I: IntoIterator<Item = (N, ProfileValue)>,
        N: Into<String>,
    {
        Self {
            client_request_id: client_request_id.into(),
            capsule,
            profile,
            inputs: inputs
                .into_iter()
                .map(|(name, value)| (name.into(), value))
                .collect(),
            resource_overrides: None,
        }
    }

    pub fn with_resource_overrides(mut self, overrides: ProfileResourceOverrides) -> Self {
        self.resource_overrides = Some(overrides);
        self
    }

    pub fn client_request_id(&self) -> &str {
        &self.client_request_id
    }

    pub fn capsule(&self) -> &CapsuleIdentity {
        &self.capsule
    }

    pub fn profile(&self) -> &ProfileIdentity {
        &self.profile
    }

    pub fn inputs(&self) -> impl ExactSizeIterator<Item = (&str, &ProfileValue)> {
        self.inputs
            .iter()
            .map(|(name, value)| (name.as_str(), value))
    }

    pub fn resource_overrides(&self) -> Option<&ProfileResourceOverrides> {
        self.resource_overrides.as_ref()
    }

    pub fn into_profile_parts(
        self,
    ) -> (
        ProfileIdentity,
        Vec<(String, ProfileValue)>,
        Option<ProfileResourceOverrides>,
    ) {
        (self.profile, self.inputs, self.resource_overrides)
    }

    pub fn into_profile_call(self) -> ProfileCall {
        let (identity, inputs, resource_overrides) = self.into_profile_parts();
        let call = ProfileCall::new(identity, inputs);
        match resource_overrides {
            Some(overrides) => call.with_resource_overrides(overrides),
            None => call,
        }
    }
}

/// Typed value accepted at the domain boundary before Capsule validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileValue {
    String(String),
    Int64(i64),
    Boolean(bool),
    LocalInput {
        path: String,
        digest: String,
        size_bytes: u64,
    },
}

impl ProfileValue {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::String(_) => "STRING",
            Self::Int64(_) => "INT64",
            Self::Boolean(_) => "BOOLEAN",
            Self::LocalInput { .. } => "LOCAL_INPUT",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuMaxOverride {
    quota_micros: u64,
    period_micros: u64,
}

impl CpuMaxOverride {
    pub const fn new(quota_micros: u64, period_micros: u64) -> Self {
        Self {
            quota_micros,
            period_micros,
        }
    }

    pub const fn quota_micros(self) -> u64 {
        self.quota_micros
    }

    pub const fn period_micros(self) -> u64 {
        self.period_micros
    }
}

/// Optional policy fields requested by a Capsule invocation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileResourceOverrides {
    cpu_max: Option<CpuMaxOverride>,
    memory_max_bytes: Option<u64>,
    pids_max: Option<u64>,
    wall_time_limit_ms: Option<u64>,
    stdout_tail_max_bytes: Option<u32>,
    stderr_tail_max_bytes: Option<u32>,
}

impl ProfileResourceOverrides {
    pub const fn new() -> Self {
        Self {
            cpu_max: None,
            memory_max_bytes: None,
            pids_max: None,
            wall_time_limit_ms: None,
            stdout_tail_max_bytes: None,
            stderr_tail_max_bytes: None,
        }
    }

    pub fn with_cpu_max(mut self, value: CpuMaxOverride) -> Self {
        self.cpu_max = Some(value);
        self
    }

    pub fn with_memory_max_bytes(mut self, value: u64) -> Self {
        self.memory_max_bytes = Some(value);
        self
    }

    pub fn with_pids_max(mut self, value: u64) -> Self {
        self.pids_max = Some(value);
        self
    }

    pub fn with_wall_time_limit_ms(mut self, value: u64) -> Self {
        self.wall_time_limit_ms = Some(value);
        self
    }

    pub fn with_stdout_tail_max_bytes(mut self, value: u32) -> Self {
        self.stdout_tail_max_bytes = Some(value);
        self
    }

    pub fn with_stderr_tail_max_bytes(mut self, value: u32) -> Self {
        self.stderr_tail_max_bytes = Some(value);
        self
    }

    pub const fn cpu_max(&self) -> Option<CpuMaxOverride> {
        self.cpu_max
    }

    pub const fn memory_max_bytes(&self) -> Option<u64> {
        self.memory_max_bytes
    }

    pub const fn pids_max(&self) -> Option<u64> {
        self.pids_max
    }

    pub const fn wall_time_limit_ms(&self) -> Option<u64> {
        self.wall_time_limit_ms
    }

    pub const fn stdout_tail_max_bytes(&self) -> Option<u32> {
        self.stdout_tail_max_bytes
    }

    pub const fn stderr_tail_max_bytes(&self) -> Option<u32> {
        self.stderr_tail_max_bytes
    }

    pub const fn is_empty(&self) -> bool {
        self.cpu_max.is_none()
            && self.memory_max_bytes.is_none()
            && self.pids_max.is_none()
            && self.wall_time_limit_ms.is_none()
            && self.stdout_tail_max_bytes.is_none()
            && self.stderr_tail_max_bytes.is_none()
    }
}

/// Shell-free argv element after scalar binding but before runtime paths exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifiedArgument {
    Literal(String),
    InputArtifactPath { slot: String },
    OutputArtifactPath { slot: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invocation_keeps_capsule_and_profile_as_distinct_domain_values() {
        let capsule = CapsuleIdentity::new("media.extract-audio", "1.0.0").unwrap();
        let invocation = CapsuleInvocation::new(
            "22222222-2222-4222-8222-222222222222",
            capsule.clone(),
            ProfileIdentity::new("media.extract-audio", "1.0.0"),
            [("channels", ProfileValue::Int64(1))],
        );

        assert_eq!(invocation.capsule(), &capsule);
        assert_eq!(invocation.profile().name(), capsule.name());
        assert_eq!(invocation.inputs().len(), 1);
    }
}
