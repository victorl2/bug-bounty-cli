//! Options every command shares, plus the checks that apply to them.

use std::path::PathBuf;

use clap::Args;

use crate::cache::env_ttl_hours;
use crate::error::{Error, Result};
use crate::render::Style;

#[derive(Args, Debug, Clone, Default)]
#[command(next_help_heading = "General options")]
pub struct Common {
    /// machine-readable output
    #[arg(long)]
    pub json: bool,
    /// ignore the cache and fetch again
    #[arg(long)]
    pub refresh: bool,
    /// use cached data only
    #[arg(long)]
    pub offline: bool,
    /// cache lifetime (default: 12)
    #[arg(long, value_name = "HOURS")]
    pub ttl: Option<f64>,
    /// cache root (one subdirectory per platform, plus evm/)
    #[arg(long, value_name = "DIR")]
    pub cache_dir: Option<PathBuf>,
    /// plain output without colours
    #[arg(long)]
    pub no_color: bool,
    /// Firefox profile with a signed-in session, for platforms that need one
    #[arg(long, value_name = "PATH|auto")]
    pub firefox_profile: Option<String>,
}

impl Common {
    /// Fill defaults from the environment and reject impossible combinations.
    pub fn validated(mut self, legacy_ttl_env: &[&str]) -> Result<Self> {
        if self.refresh && self.offline {
            return Err(Error::value("--refresh and --offline cannot be combined"));
        }
        let ttl = self.ttl.unwrap_or_else(|| env_ttl_hours(legacy_ttl_env));
        if !ttl.is_finite() || ttl < 0.0 {
            return Err(Error::value("TTL must be finite and nonnegative"));
        }
        self.ttl = Some(ttl);
        if self.firefox_profile.is_none() {
            self.firefox_profile =
                ["BB_FIREFOX_PROFILE", "CANTINA_FIREFOX_PROFILE"].iter().find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()));
        }
        Ok(self)
    }

    pub fn ttl_hours(&self) -> f64 {
        self.ttl.unwrap_or(crate::cache::DEFAULT_TTL_HOURS)
    }

    pub fn style(&self) -> Style {
        Style::for_stdout(self.no_color)
    }
}

/// Print JSON the way every command does: two-space indent, Unicode kept.
pub fn print_json(value: &impl serde::Serialize) -> Result<i32> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(0)
}

/// The two kinds of opportunity every platform lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Program,
    Competition,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Program => "program",
            Kind::Competition => "competition",
        }
    }

    pub fn other(self) -> Kind {
        match self {
            Kind::Program => Kind::Competition,
            Kind::Competition => Kind::Program,
        }
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
