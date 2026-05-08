//! `oxideav-core` integration.
//!
//! Wired up only when the default `registry` cargo feature is on.
//! Standalone consumers (no oxideav-core dep) skip this module and
//! reach the demuxer types directly via [`crate::MovDemuxer`].

use crate::demuxer;

use oxideav_core::ContainerRegistry;

/// Install the QTFF demuxer into a [`ContainerRegistry`].
///
/// Registers:
///
/// * `mov` demuxer factory
/// * `mov` / `qt` filename extensions → `mov` container
/// * `mov` content probe (recognises `ftyp qt  ` and `ftyp ...
///   compat: qt  ` patterns)
pub fn register_containers(reg: &mut ContainerRegistry) {
    reg.register_demuxer("mov", demuxer::open);
    reg.register_extension("mov", "mov");
    reg.register_extension("qt", "mov");
    reg.register_probe("mov", demuxer::probe);
}

/// Install the QTFF demuxer into an
/// [`oxideav_core::RuntimeContext`]. Convenience wrapper around
/// [`register_containers`] that matches the uniform
/// `register(&mut RuntimeContext)` entry point every sibling crate
/// exposes; `oxideav_meta::register_all` calls
/// `crate::__oxideav_entry(ctx)` which dispatches here.
pub fn register(ctx: &mut oxideav_core::RuntimeContext) {
    register_containers(&mut ctx.containers);
}

oxideav_core::register!("mov", register);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_via_runtime_context_installs_container() {
        let mut ctx = oxideav_core::RuntimeContext::new();
        register(&mut ctx);
        assert_eq!(ctx.containers.container_for_extension("mov"), Some("mov"));
        assert_eq!(ctx.containers.container_for_extension("qt"), Some("mov"));
    }
}
