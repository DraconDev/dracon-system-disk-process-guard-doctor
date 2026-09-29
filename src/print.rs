//! Human-friendly print helpers shared across dracon-system commands.
//!
//! NO_COLOR spec: <https://no-color.org/> — if the env var is set (to anything,
//! including empty), colour MUST be disabled.

/// Render a boolean as a compact on/off string for tables and flags rows.
pub fn onoff(b: bool) -> &'static str {
    if b {
        "on"
    } else {
        "off"
    }
}
