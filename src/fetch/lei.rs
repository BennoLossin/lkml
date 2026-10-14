use std::{
    path::Path,
    process::Command,
};

use tracing::debug;

use crate::fetch::{
    Error,
    Fetch,
    Result,
};

pub fn fetch(cfg: &Fetch, dir: &Path) -> Result<()> {
    let mut cmd = Command::new("lei");
    cmd.arg("q")
        .args([
            // don't store the query, as we're storing it in our config.
            "--no-save",
            // get all emails from the thread where a single one has matched.
            "--threads",
            "--include",
            cfg.inbox,
        ])
        .arg(format!("--output={}", dir.display()))
        .arg(cfg.query);
    debug!("{cmd:?}");
    let res = cmd.status()?;
    if !res.success() {
        Err(res.code().map(Error::Code).unwrap_or(Error::Signal))
    } else {
        Ok(())
    }
}
