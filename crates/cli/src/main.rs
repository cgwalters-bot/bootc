//! The main entrypoint for bootc, which just performs global initialization, and then
//! calls out into the library.
//!
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rustix::{
    mm::{MlockAllFlags, mlockall},
    process::{Resource, getrlimit},
};

const MLOCK_ENV: &str = "BOOTC_EXPERIMENTAL_MLOCK_VIRTIOFS";
const STATUS_PATH: &str = "/proc/self/status";
const MOUNTINFO_PATH: &str = "/proc/self/mountinfo";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MlockMode {
    Auto,
    Force,
}

impl MlockMode {
    fn from_env(value: Option<&str>) -> Result<Option<Self>> {
        match value {
            None => Ok(None),
            Some("auto") => Ok(Some(Self::Auto)),
            Some("force") => Ok(Some(Self::Force)),
            Some(value) => bail!("Invalid {MLOCK_ENV}={value:?}; expected one of: auto, force"),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ExecutableBacking {
    Virtiofs,
    Other(String),
    OverlayUnknown,
    Unknown,
}

fn should_activate(mode: MlockMode, backing: &ExecutableBacking) -> bool {
    matches!(mode, MlockMode::Force) || matches!(backing, ExecutableBacking::Virtiofs)
}

fn unescape_mountinfo_path(value: &str) -> Result<String> {
    let mut result = String::new();
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            result.push(character);
            continue;
        }
        let digits: String = chars.by_ref().take(3).collect();
        let value = u8::from_str_radix(&digits, 8)
            .with_context(|| format!("Invalid mountinfo escape in {value:?}"))?;
        result.push(char::from(value));
    }
    Ok(result)
}

fn executable_backing(mountinfo: &str, executable: &Path) -> Result<ExecutableBacking> {
    let executable = executable.to_string_lossy();
    let mut selected: Option<(usize, &str)> = None;
    for (line_number, line) in mountinfo.lines().enumerate() {
        let (before_separator, after_separator) = line
            .split_once(" - ")
            .with_context(|| format!("Mountinfo line {} lacks separator", line_number + 1))?;
        let fields: Vec<_> = before_separator.split_ascii_whitespace().collect();
        let mountpoint = fields
            .get(4)
            .context("Mountinfo line lacks mountpoint")
            .and_then(|value| unescape_mountinfo_path(value))?;
        let is_prefix = mountpoint == "/"
            || executable
                .strip_prefix(&mountpoint)
                .is_some_and(|remaining| remaining.starts_with('/'));
        if !is_prefix {
            continue;
        }
        let filesystem = after_separator
            .split_ascii_whitespace()
            .next()
            .context("Mountinfo line lacks filesystem type")?;
        if selected.is_none_or(|(length, _)| mountpoint.len() > length) {
            selected = Some((mountpoint.len(), filesystem));
        }
    }
    match selected {
        Some((_, "virtiofs")) => Ok(ExecutableBacking::Virtiofs),
        // Resolving overlay lowerdirs across namespaces is deliberately avoided:
        // a false positive would silently turn a control into a treatment.
        Some((_, "overlay")) => Ok(ExecutableBacking::OverlayUnknown),
        Some((_, filesystem)) => Ok(ExecutableBacking::Other(filesystem.to_owned())),
        None => Ok(ExecutableBacking::Unknown),
    }
}

fn read_vmlck(status: &str) -> Result<u64> {
    let value = status
        .lines()
        .find_map(|line| line.strip_prefix("VmLck:"))
        .context("VmLck is missing from /proc/self/status")?
        .split_ascii_whitespace()
        .next()
        .context("VmLck has no value in /proc/self/status")?;
    value
        .parse::<u64>()
        .context("Parsing VmLck from /proc/self/status")
}

fn experimental_mlock_virtiofs() -> Result<()> {
    let mode = match std::env::var(MLOCK_ENV) {
        Ok(value) => MlockMode::from_env(Some(&value))?,
        Err(std::env::VarError::NotPresent) => MlockMode::from_env(None)?,
        Err(std::env::VarError::NotUnicode(_)) => {
            bail!("Invalid {MLOCK_ENV}: value is not valid UTF-8")
        }
    };
    let Some(mode) = mode else {
        return Ok(());
    };

    let executable = bootc_utils::reexec::executable_path()
        .context("Resolving the original bootc executable")?;
    let backing = match std::fs::read_to_string(MOUNTINFO_PATH) {
        Ok(mountinfo) => match executable_backing(&mountinfo, &executable) {
            Ok(backing) => backing,
            Err(error) => {
                tracing::warn!(%error, "experimental virtiofs mlock could not parse mountinfo; treating backing as unknown");
                ExecutableBacking::Unknown
            }
        },
        Err(error) => {
            tracing::warn!(%error, "experimental virtiofs mlock could not read mountinfo; treating backing as unknown");
            ExecutableBacking::Unknown
        }
    };
    let limit = getrlimit(Resource::Memlock);
    let before = read_vmlck(&std::fs::read_to_string(STATUS_PATH)?)?;
    let activate = should_activate(mode, &backing);
    tracing::info!(?mode, executable = %executable.display(), ?backing,
        memlock_soft = ?limit.current, memlock_hard = ?limit.maximum, vmlck_before_kib = before,
        activate, "experimental virtiofs mlock determination");
    if !activate {
        tracing::info!(
            ?mode,
            ?backing,
            vmlck_after_kib = before,
            "experimental virtiofs mlock skipped: backing is unknown or not direct virtiofs"
        );
        return Ok(());
    }

    let started = Instant::now();
    let started_at_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    // rustix exposes this syscall safely. The invariant is that CURRENT is the
    // sole flag: this experiment never locks future or on-fault mappings.
    tracing::info!(?mode, executable = %executable.display(), ?backing,
        memlock_soft = ?limit.current, memlock_hard = ?limit.maximum, vmlck_before_kib = before,
        started_at_unix_ms, "BOOTC_EXPERIMENTAL_MLOCK_VIRTIOFS MLOCKALL_BEGIN");
    mlockall(MlockAllFlags::CURRENT)
        .with_context(|| {
            format!(
                "Experimental virtiofs mlockall(MCL_CURRENT) failed: mode={mode:?} memlock_soft={:?} memlock_hard={:?} vmlck_before_kib={before}",
                limit.current, limit.maximum
            )
        })?;
    let after = read_vmlck(&std::fs::read_to_string(STATUS_PATH)?)?;
    if after <= before {
        bail!(
            "Experimental virtiofs mlockall(MCL_CURRENT) did not increase VmLck: before={before} KiB after={after} KiB"
        );
    }
    let completed_at_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    tracing::info!(?mode, executable = %executable.display(), ?backing,
        memlock_soft = ?limit.current, memlock_hard = ?limit.maximum, vmlck_before_kib = before,
        vmlck_after_kib = after, elapsed_ms = started.elapsed().as_millis(), completed_at_unix_ms,
        "BOOTC_EXPERIMENTAL_MLOCK_VIRTIOFS ACTIVATED");
    Ok(())
}

/// The code called after we've done process global init and created
/// an async runtime.
async fn async_main() -> Result<()> {
    bootc_utils::initialize_tracing();

    // This is repeated after a SELinux self-reexec because every process begins here.
    experimental_mlock_virtiofs()?;

    tracing::trace!("starting bootc");

    // As you can see, the role of this file is mostly to just be a shim
    // to call into the code that lives in the internal shared library.
    bootc_lib::cli::run_from_iter(std::env::args()).await
}

/// Perform process global initialization, then create an async runtime
/// and do the rest of the work there.
fn run() -> Result<()> {
    // Initialize global state before we've possibly created other threads, etc.
    bootc_lib::cli::global_init()?;
    // We only use the "current thread" runtime because we don't perform
    // a lot of CPU heavy work in async tasks. Where we do work on the CPU,
    // or we do want explicit concurrency, we typically use
    // tokio::task::spawn_blocking to create a new OS thread explicitly.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to build tokio runtime");
    // And invoke the async_main
    runtime.block_on(async move { async_main().await })
}

fn main() {
    bootc_utils::run_main(run)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mlock_mode_from_env() {
        for (input, expected) in [
            (None, None),
            (Some("auto"), Some(MlockMode::Auto)),
            (Some("force"), Some(MlockMode::Force)),
        ] {
            assert_eq!(MlockMode::from_env(input).unwrap(), expected);
        }
        assert!(MlockMode::from_env(Some("yes")).is_err());
    }

    #[test]
    fn parse_vmlck() {
        for (status, expected) in [
            ("Name:\tbootc\nVmLck:\t       0 kB\n", Ok(0)),
            ("VmLck:\t  1234 kB\n", Ok(1234)),
            ("Name:\tbootc\n", Err(())),
            ("VmLck:\t nope kB\n", Err(())),
        ] {
            assert_eq!(read_vmlck(status).map_err(|_| ()), expected);
        }
    }

    #[test]
    fn detect_executable_backing() {
        for (mountinfo, executable, expected) in [
            (
                "25 1 0:24 / / rw - virtiofs host rw\n",
                "/usr/bin/bootc",
                ExecutableBacking::Virtiofs,
            ),
            (
                "25 1 0:24 / / rw - ext4 /dev/vda rw\n",
                "/usr/bin/bootc",
                ExecutableBacking::Other("ext4".into()),
            ),
            (
                "25 1 0:24 / / rw - overlay overlay rw\n",
                "/usr/bin/bootc",
                ExecutableBacking::OverlayUnknown,
            ),
            (
                "25 1 0:24 / / rw - ext4 /dev/vda rw\n26 25 0:25 / /usr rw - virtiofs host rw\n",
                "/usr/bin/bootc",
                ExecutableBacking::Virtiofs,
            ),
            (
                "25 1 0:24 / / rw - ext4 /dev/vda rw\n",
                "/opt/bootc",
                ExecutableBacking::Other("ext4".into()),
            ),
            (
                "25 1 0:24 / /opt\\040tools rw - virtiofs host rw\n",
                "/opt tools/bin/bootc",
                ExecutableBacking::Virtiofs,
            ),
        ] {
            assert_eq!(
                executable_backing(mountinfo, Path::new(executable)).unwrap(),
                expected
            );
        }
        assert!(executable_backing("malformed\n", Path::new("/usr/bin/bootc")).is_err());
    }

    #[test]
    fn choose_mlock_activation() {
        for (mode, backing, expected) in [
            (MlockMode::Auto, ExecutableBacking::Virtiofs, true),
            (MlockMode::Auto, ExecutableBacking::OverlayUnknown, false),
            (MlockMode::Auto, ExecutableBacking::Unknown, false),
            (
                MlockMode::Auto,
                ExecutableBacking::Other("ext4".into()),
                false,
            ),
            (MlockMode::Force, ExecutableBacking::Virtiofs, true),
            (MlockMode::Force, ExecutableBacking::OverlayUnknown, true),
            (MlockMode::Force, ExecutableBacking::Unknown, true),
        ] {
            assert_eq!(should_activate(mode, &backing), expected);
        }
    }
}
