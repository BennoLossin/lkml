use std::io;

use clap::ValueEnum;
use tempdir::TempDir;
use thiserror::Error;
use tracing::debug;

use crate::config::FetchMethod;

mod lei;

const DEFAULT_INBOX: &str = "https://lore.kernel.org/all/";

type Result<T = ()> = core::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error(
        "could not execute `lei`: {0}. Consider using the `no_lei` config option."
    )]
    Start(#[from] io::Error),
    #[error("`lei` failed execution with error code {0}")]
    Code(i32),
    #[error("`lei` execution unexpectedly terminated by signal.")]
    Signal,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum Interval {
    /// Searches mails up to 2 days ago.
    Day,
    /// Searches mails up to 2 weeks ago.
    Week,
    /// Searches mails up to 3 months ago.
    Month,
    /// Searches mails up to 1 year ago.
    Year,
}

pub fn fetch(
    interval: Interval,
    query: &str,
    fetch_method: FetchMethod,
) -> Result<TempDir> {
    let interval = match interval {
        Interval::Day => "2.day.ago",
        Interval::Week => "2.week.ago",
        Interval::Month => "3.month.ago",
        Interval::Year => "1.year.ago",
    };
    let tmpdir = TempDir::new("lkml-lei")?;
    let query = format!("({query}) AND rt:{interval}..");
    debug!("query: `{query}`");

    let cfg = Fetch {
        query: &query,
        inbox: DEFAULT_INBOX,
    };

    match fetch_method {
        FetchMethod::Lei => lei::fetch(&cfg, tmpdir.path()),
    }
    .map(|()| tmpdir)
}

#[derive(Debug)]
pub struct Fetch<'a> {
    /// public-inbox query.
    pub query: &'a str,

    /// The lore server address.
    pub inbox: &'a str,
}
