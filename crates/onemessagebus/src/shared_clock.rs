//! The clock two processes share, so a deadline one of them took as an
//! [`Instant`] means the same moment to the other.
//!
//! On Linux that is `CLOCK_MONOTONIC`, the clock `Instant` reads. It counts
//! from the boot, carries its time namespace's offset, and no wall-clock step
//! moves it, so two processes read the same value of it exactly when they share
//! the boot and the time namespace. A clock is named by both. Where no such
//! clock can be identified — off Linux, or with `/proc` unreadable — the shared
//! clock is the system clock, which every process on a host reads alike but
//! which a step of the wall clock moves.

#[cfg(target_os = "linux")]
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Which clock a reading is on, written
/// `linux-monotonic:<boot id>:<time namespace inode>` (`…:none` on a kernel
/// without time namespaces), or `system`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub(crate) enum ClockIdentity {
    /// `CLOCK_MONOTONIC` on one Linux boot, in one time namespace.
    Monotonic {
        /// The kernel's boot id: a UUID, new at every boot.
        boot: String,
        /// The inode of the time namespace the clock is read in, whose offset
        /// it carries; `None` on a kernel without time namespaces, where every
        /// process reads the clock alike.
        namespace: Option<u64>,
    },
    /// The system clock, in nanoseconds since the Unix epoch.
    System,
}

const MONOTONIC: &str = "linux-monotonic:";
const SYSTEM: &str = "system";

impl From<ClockIdentity> for String {
    fn from(identity: ClockIdentity) -> Self {
        match identity {
            ClockIdentity::Monotonic { boot, namespace } => {
                let namespace =
                    namespace.map_or_else(|| "none".to_owned(), |inode| inode.to_string());
                format!("{MONOTONIC}{boot}:{namespace}")
            }
            ClockIdentity::System => SYSTEM.to_owned(),
        }
    }
}

impl TryFrom<String> for ClockIdentity {
    type Error = String;

    fn try_from(text: String) -> Result<Self, String> {
        let refused = || {
            format!(
                "{text:?} is not a clock identity: \
                 `linux-monotonic:<boot id>:<time namespace inode>`, `…:none` or `system`"
            )
        };
        if text == SYSTEM {
            return Ok(Self::System);
        }
        let (boot, namespace) = text
            .strip_prefix(MONOTONIC)
            .and_then(|rest| rest.split_once(':'))
            .ok_or_else(refused)?;
        if !is_boot_id(boot) {
            return Err(refused());
        }
        let namespace = match namespace {
            "none" => None,
            inode => Some(inode.parse().map_err(|_| refused())?),
        };
        Ok(Self::Monotonic {
            boot: boot.to_owned(),
            namespace,
        })
    }
}

/// Whether `text` is a boot id as the kernel writes it: a lowercase UUID.
fn is_boot_id(text: &str) -> bool {
    text.len() == 36
        && text.char_indices().all(|(at, character)| match at {
            8 | 13 | 18 | 23 => character == '-',
            _ => matches!(character, '0'..='9' | 'a'..='f'),
        })
}

/// The clock this process shares with the others on its host.
#[derive(Debug, Clone)]
pub(crate) struct SharedClock {
    identity: ClockIdentity,
}

impl SharedClock {
    /// This process's shared clock: `CLOCK_MONOTONIC` where its identity can
    /// be read, the system clock otherwise.
    pub(crate) fn here() -> Self {
        Self {
            identity: monotonic().unwrap_or(ClockIdentity::System),
        }
    }

    pub(crate) fn identity(&self) -> &ClockIdentity {
        &self.identity
    }

    /// Whether `other` names this same clock.
    pub(crate) fn is(&self, other: &ClockIdentity) -> bool {
        self.identity == *other
    }

    /// `deadline` on this clock, in nanoseconds. `Instant` is read first, so
    /// the reading is never earlier than the deadline it stands for.
    pub(crate) fn at(&self, deadline: Instant) -> u64 {
        let left = deadline.saturating_duration_since(Instant::now());
        self.now()
            .saturating_add(u64::try_from(left.as_nanos()).unwrap_or(u64::MAX))
    }

    /// Now on this clock, in nanoseconds.
    pub(crate) fn now(&self) -> u64 {
        match self.identity {
            ClockIdentity::Monotonic { .. } => monotonic_now(),
            ClockIdentity::System => SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| {
                    u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
                }),
        }
    }
}

#[cfg(target_os = "linux")]
fn monotonic() -> Option<ClockIdentity> {
    static HERE: OnceLock<Option<ClockIdentity>> = OnceLock::new();
    HERE.get_or_init(identify).clone()
}

#[cfg(not(target_os = "linux"))]
fn monotonic() -> Option<ClockIdentity> {
    None
}

#[cfg(target_os = "linux")]
fn monotonic_now() -> u64 {
    let reading = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    let seconds = u64::try_from(reading.tv_sec).unwrap_or(0);
    let nanos = u64::try_from(reading.tv_nsec).unwrap_or(0);
    seconds.saturating_mul(1_000_000_000).saturating_add(nanos)
}

/// Never called: off Linux no monotonic identity is made.
#[cfg(not(target_os = "linux"))]
fn monotonic_now() -> u64 {
    0
}

#[cfg(target_os = "linux")]
fn identify() -> Option<ClockIdentity> {
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    let namespace = match std::fs::read_link("/proc/self/ns/time") {
        Ok(link) => Some(
            link.to_str()?
                .strip_prefix("time:[")?
                .strip_suffix(']')?
                .parse()
                .ok()?,
        ),
        // A kernel without time namespaces: one clock for every process.
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return None,
    };
    let boot = boot.trim();
    is_boot_id(boot).then(|| ClockIdentity::Monotonic {
        boot: boot.to_owned(),
        namespace,
    })
}
