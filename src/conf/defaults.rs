use crate::ContextNature;

pub static DEFAULT_PREFS: &str = include_str!("../../defaults/default-prefs.toml");

pub static DEFAULT_PACKAGE_CONFIG: &str = include_str!("../../defaults/default-bacon.toml");

pub static DEFAULT_CSHARP_PACKAGE_CONFIG: &str =
    include_str!("../../defaults/default-bacon-csharp.toml");

/// The default `bacon.toml` content fitting the nature of the project.
///
/// This is both what's applied under any user configuration and what
/// `bacon --init` writes.
pub fn default_package_config_str(nature: ContextNature) -> &'static str {
    match nature {
        ContextNature::Csharp => DEFAULT_CSHARP_PACKAGE_CONFIG,
        ContextNature::Cargo | ContextNature::Other => DEFAULT_PACKAGE_CONFIG,
    }
}
