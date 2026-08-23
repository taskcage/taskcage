//! Backend-independent Capsule identity and invocation values.

mod identity;
mod invocation;

pub use identity::{CapsuleIdentity, IdentityError, is_valid_capsule_name};
pub use invocation::{
    CapsuleInvocation, CpuMaxOverride, ProfileCall, ProfileIdentity, ProfileResourceOverrides,
    ProfileValue, VerifiedArgument,
};
