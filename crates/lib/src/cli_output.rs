//! Human-facing status output for CLI subcommands.
//!
//! Subcommand implementations (install, upgrade, switch, ...) print
//! progress and status lines meant for a person at a terminal. Route those
//! through `cli_status!` and `cli_warn!` rather than calling
//! `println!`/`eprintln!` directly, so that direct printing elsewhere in the
//! library stands out.
//!
//! Output that is the actual result of a command (JSON, digests, tables)
//! should instead be written to an explicit `impl Write`, and diagnostics
//! should use `tracing`.

use std::fmt::Arguments;

/// Print a human-readable status line to stdout.
macro_rules! cli_status {
    ($($arg:tt)*) => {
        $crate::cli_output::status(format_args!($($arg)*))
    };
}

/// Print a human-readable warning line to stderr, regardless of the
/// `tracing` log level.
macro_rules! cli_warn {
    ($($arg:tt)*) => {
        $crate::cli_output::warn(format_args!($($arg)*))
    };
}

/// Implementation of `cli_status!`.
#[expect(clippy::print_stdout, reason = "this is the CLI status output helper")]
pub(crate) fn status(args: Arguments<'_>) {
    println!("{args}");
}

/// Implementation of `cli_warn!`.
#[expect(clippy::print_stderr, reason = "this is the CLI warning output helper")]
pub(crate) fn warn(args: Arguments<'_>) {
    eprintln!("{args}");
}
