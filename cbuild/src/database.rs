use std::path::{Path, PathBuf};
use serde::{Serialize, ser::SerializeSeq};

#[derive(Debug, Serialize)]
pub struct Entry {
    pub directory: PathBuf,
    pub file: PathBuf,
    pub output: PathBuf,
    pub arguments: Vec<String>
}

#[derive(Debug)]
pub struct Database {
    pub entries: Vec<Entry>
}

impl Serialize for Database {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer {
        let mut seq = serializer.serialize_seq(Some(self.entries.len()))?;
        for entry in &self.entries {
            seq.serialize_element(entry)?;
        }
        seq.end()
    }
}
