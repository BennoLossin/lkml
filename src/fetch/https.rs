use std::{
    fmt::Display,
    fs,
    io::{
        self,
        BufRead,
        BufReader,
        Read,
        Write,
    },
    path::{
        Path,
        PathBuf,
    },
    time::Instant,
};

use flate2::read::GzDecoder;
use thiserror::Error;
use tracing::{
    debug,
    info,
};

use super::Fetch;
use crate::util::{
    self,
    ReadCounter,
};

#[derive(Debug, Error)]
pub enum Error {
    #[error("could not reach `{inbox}`: {error}")]
    Http { inbox: Box<str>, error: ureq::Error },
    #[error("could not create directory `{path}`: {error}")]
    CreateDirectory { path: Box<Path>, error: io::Error },
    #[error("could not write new mail file `{path}`: {error}")]
    WriteMailFile { path: Box<Path>, error: io::Error },
    #[error("failed to read from response {error}")]
    ReadInputStream { error: io::Error },
}

type Result<T = ()> = core::result::Result<T, Error>;

/// Every N bytes, post an update.
const MSG_INTERVAL: u64 = 10 * 1024;

const PATH_SEP: char = if cfg!(windows) { ';' } else { ':' };

pub fn fetch(cfg: &Fetch, dir: &Path) -> super::Result<()> {
    let (body_reader, start) = email_reader(cfg)?;
    let maildir = create_maildir(dir)?;

    // lei uses the git hash of a message as its filename, plus the maildir
    // `:2,` ending.
    let create_fname: fn(&[u8]) -> String = |bytes| {
        let hash = git_hash_object(bytes);
        format!("{hash}{PATH_SEP}2,")
    };

    let res = mbox2mdir(
        body_reader,
        &maildir,
        create_fname,
        |r| r.get_ref().get_ref().get_ref().count(),
        |r| r.get_ref().count(),
    );
    let elapsed = Instant::now() - start;
    println!("Download completed in {elapsed:?}");
    Ok(res?)
}

fn email_reader(
    cfg: &Fetch,
) -> Result<(
    BufReader<ReadCounter<GzDecoder<ReadCounter<impl Read>>>>,
    Instant,
)> {
    // A POST with `?x=m` requests to download messages
    let mut req = ureq::post(cfg.inbox).query("x", "m");
    if cfg.full_thread {
        req = req.query("t", "1");
    }
    req = req.query("q", cfg.query);

    let start = Instant::now();
    info!("sending request {req:?}");
    let resp = req.send_empty().map_err(|e| Error::Http {
        inbox: cfg.inbox.into(),
        error: e,
    })?;

    // Lore doesn't seem to set `Content-Length`, but we still try
    let expected_len: Option<u64> = resp
        .headers()
        .get("Content-Length")
        .and_then(|hval| hval.to_str().ok())
        .and_then(|hstr| hstr.parse().ok());

    let status = resp.status();
    if let Some(l) = expected_len {
        info!("status {status}; receiving {l} bytes",);
    } else {
        info!("status {status}; unknown length",);
    };

    // Wrap the response in a `GzDecoder` so we can decompress on the fly, plus
    // a `BufReader` since we read individual lines. `ReadCounter`s are inserted
    // so we can separately log how much has been downloaded and decompressed.
    let body_reader = BufReader::new(ReadCounter::new(GzDecoder::new(
        ReadCounter::new(resp.into_body().into_reader()),
    )));
    Ok((body_reader, start))
}

/// Given an input stream that produces `mbox`, split it into individual
/// messages.
///
/// Takes callbacks so we can test this without the ureq stream.
///
/// Based on
/// <https://github.com/mindbit/mb2md/blob/52d9a9480f521a1e3dda83a0845e6ccfa84e54aa/mb2md.pl>.
fn mbox2mdir<R: BufRead>(
    mut r: R,
    maildir: &Path,
    mut create_fname: impl FnMut(&[u8]) -> String,
    get_downloaded: impl Fn(&R) -> u64,
    get_extracted: impl Fn(&R) -> u64,
) -> Result<()> {
    let mut buf = Vec::new();
    let mut current_msg = Vec::with_capacity(1024);
    let mut total_read = 0u64;
    let mut messages = 0u64;
    let mut duplicates = 0u64;

    loop {
        // Read a single line
        buf.clear();
        let read_count = r
            .read_until(b'\n', &mut buf)
            .map_err(|error| Error::ReadInputStream { error })?;
        let line = &buf[..read_count];
        let eof = read_count == 0;
        let start_of_msg = line.starts_with(b"From mboxrd@z");

        let read_count_u64 = u64::try_from(read_count).unwrap();
        let new_total_read = total_read + read_count_u64;
        if new_total_read / MSG_INTERVAL > total_read / MSG_INTERVAL {
            let downloaded = get_downloaded(&r);
            let extracted = get_extracted(&r);
            print!(
                "\r{downloaded} B downloaded, {extracted} B extracted, {new_total_read} B read, \
                {messages} messages "
            );
        }

        total_read = new_total_read;

        // This logic is taken from mb2md's `convert` function (not all
        // functionality is implemented)

        if start_of_msg || eof {
            if !current_msg.is_empty() {
                append_message(
                    &mut current_msg,
                    &mut create_fname,
                    maildir,
                    &mut duplicates,
                    &mut messages,
                )?;
            }

            if read_count == 0 {
                // Reached the end of the stream
                break;
            }

            continue;
        }
        // mbox uses `From ` as the separator and quotes it as `> From`.
        // lore uses the `mboxrd` format, which also quotes `> From` as
        // `>> From` and so on. thus strip the first quote level.
        let line = match line.iter().position(|c| *c != b'>') {
            Some(pos) if pos != 0 && line[pos..].starts_with(b"From ") => {
                &line[1..]
            }
            _ => line,
        };
        current_msg.extend_from_slice(line);
    }

    let downloaded = get_downloaded(&r);
    let extracted = get_extracted(&r);
    println!(
        "\rComplete: {downloaded} B downloaded, {extracted} B extracted, {total_read} B read"
    );
    println!(
        "{} messages retrieved ({messages} matches, {duplicates} duplicates)",
        messages - duplicates,
    );
    Ok(())
}

fn append_message(
    current_msg: &mut Vec<u8>,
    create_fname: &mut impl FnMut(&[u8]) -> String,
    maildir: &Path,
    duplicates: &mut u64,
    messages: &mut u64,
) -> Result<()> {
    // Due to the `read_until` call above, we should have a trailing `\n`.
    // TODO: does mbox always have a trailing `\n` before EOF that we can strip?
    // Our output for the last message does seem to match `lei`
    assert_eq!(
        current_msg.pop(),
        Some(b'\n'),
        "possibly received broken mbox"
    );
    let to_write = &current_msg;

    let fname = create_fname(to_write);
    let fpath = maildir.join(&fname);

    // Write the file, failing if it exists
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&fpath)
        .and_then(|mut f| f.write_all(to_write))
        .or_else(|error| match error.kind() {
            // lei deduplicates messages with the same hash
            // automatically
            io::ErrorKind::AlreadyExists => {
                debug!("duplicate message `{fname}`");
                *duplicates += 1;
                Ok(())
            }
            _ => Err(error),
        })
        .map_err(|error| Error::WriteMailFile { path: fpath.into(), error })?;

    *messages += 1;
    current_msg.clear();
    Ok(())
}

/// Create a new mail directory structure, returning the `cur` path.
fn create_maildir(dir: &Path) -> Result<PathBuf> {
    let cur = dir.join("cur");
    create_dir_if_not_exists(dir)?;
    create_dir_if_not_exists(&dir.join("tmp"))?;
    create_dir_if_not_exists(&dir.join("new"))?;
    create_dir_if_not_exists(&cur)?;
    Ok(cur)
}

/// Wrap the function in `util` with a specific error type.
fn create_dir_if_not_exists(path: &Path) -> Result<()> {
    util::create_dir_if_not_exists(path)
        .map_err(|error| Error::CreateDirectory { path: path.into(), error })
}

/// Compute `git hash-object` output without calling `git`.
fn git_hash_object(bytes: &[u8]) -> impl Display + 'static {
    let mut m = sha1_smol::Sha1::new();
    let pfx = format!("blob {}\0", bytes.len());
    m.update(pfx.as_bytes());
    m.update(bytes);
    m.digest()
}

#[cfg(test)]
mod tests {
    use std::{
        io::{
            Cursor,
            Read,
        },
        process::{
            Command,
            Stdio,
        },
    };

    use indoc::indoc;
    use pretty_assertions::assert_eq;
    use tempdir::TempDir;

    use super::*;

    const MBOX: &str = indoc! {r#"
        From mboxrd@z Thu Jan  1 00:00:00 1970
        X-Some-Header: HeaderValue
        From: Benno Lossin <b@l.org>
        To: Trevor Gross <t@g.org>
        Subject: Subject

        Body1

        From mboxrd@z Thu Jan  1 00:00:00 1970
        From: Trevor Gross <t@g.org>
        To: Benno Lossin <b@l.org>
        Subject: Subject

        Body2
        This is mbox quoting, should turn into `From `:
        >From hi

        From mboxrd@z Thu Jan  1 00:00:00 1970
        From: mail@email.org
        To: List <list@mail.org>
        Subject: Subject3

        Body3

    "#};

    #[test]
    fn basic_split() {
        let dir = TempDir::new("test-mbox").unwrap();
        let mut idx = 0;

        mbox2mdir(
            Cursor::new(MBOX),
            dir.path(),
            |_bytes| {
                let name = format!("name{idx}");
                idx += 1;
                name
            },
            |_| 0,
            |_| 0,
        )
        .unwrap();

        for entry in fs::read_dir(dir.path()).unwrap() {
            let entry = entry.unwrap();
            let fname = entry.file_name();

            let expected = if fname == "name0" {
                indoc! {r#"
                    X-Some-Header: HeaderValue
                    From: Benno Lossin <b@l.org>
                    To: Trevor Gross <t@g.org>
                    Subject: Subject

                    Body1
                "#}
            } else if fname == "name1" {
                indoc! {r#"
                    From: Trevor Gross <t@g.org>
                    To: Benno Lossin <b@l.org>
                    Subject: Subject

                    Body2
                    This is mbox quoting, should turn into `From `:
                    From hi
                "#}
            } else if fname == "name2" {
                indoc! {r#"
                    From: mail@email.org
                    To: List <list@mail.org>
                    Subject: Subject3

                    Body3
                "#}
            } else {
                panic!("unexpected file {}", entry.path().display());
            };

            let contents = fs::read_to_string(entry.path()).unwrap();
            assert_eq!(contents, expected, "file: {fname:?}");
        }
    }

    #[test]
    fn hash_object() {
        fn hash_with_git(bytes: &[u8]) -> String {
            let mut git = Command::new("git")
                .args(["hash-object", "--stdin"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();

            let mut stdin = git.stdin.take().unwrap();
            let mut stdout = git.stdout.take().unwrap();
            stdin.write_all(bytes).unwrap();
            drop(stdin);
            let out = git.wait().unwrap();
            assert!(out.success());

            let mut s = String::new();
            stdout.read_to_string(&mut s).unwrap();
            s.trim().to_string()
        }

        let s = b"Hello, world!";
        assert_eq!(git_hash_object(s).to_string(), hash_with_git(s));
    }
}
