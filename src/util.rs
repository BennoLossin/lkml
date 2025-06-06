use std::{
    fs,
    io::{
        self,
        Read,
    },
    path::Path,
};

/// Add a total count to any reader.
pub struct ReadCounter<R> {
    reader: R,
    count: u64,
}

impl<R> ReadCounter<R> {
    pub fn new(reader: R) -> Self {
        Self { reader, count: 0 }
    }

    pub fn get_ref(&self) -> &R {
        &self.reader
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    /// Helper to increment the count and return the input value.
    fn increment_count(&mut self, add: usize) -> usize {
        self.count += u64::try_from(add).unwrap();
        add
    }
}

impl<R: Read> Read for ReadCounter<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.reader.read(buf)?;
        Ok(self.increment_count(n))
    }

    fn read_to_end(&mut self, buf: &mut Vec<u8>) -> io::Result<usize> {
        let n = self.reader.read_to_end(buf)?;
        Ok(self.increment_count(n))
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
        self.reader.read_exact(buf)?;
        self.increment_count(buf.len());
        Ok(())
    }
}

/// Try to create a directory, ignore already exists errors.
pub fn create_dir_if_not_exists(path: impl AsRef<Path>) -> io::Result<()> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}
