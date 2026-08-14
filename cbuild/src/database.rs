use std::path::PathBuf;
use serde::{Serialize, ser::SerializeSeq, ser::SerializeStruct};

use crate::display_path;

#[derive(Debug)]
pub struct Entry {
    pub directory: PathBuf,
    pub file: PathBuf,
    pub output: PathBuf,
    pub arguments: Vec<String>
}

impl Serialize for Entry {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("Entry", 4)?;
        state.serialize_field("directory", &display_path(&self.directory))?;
        state.serialize_field("file", &display_path(&self.file))?;
        state.serialize_field("output", &display_path(&self.output))?;
        let arguments = self
            .arguments
            .iter()
            .map(|arg| arg.replace("\\\\?\\", ""))
            .collect::<Vec<_>>();
        state.serialize_field("arguments", &arguments)?;
        state.end()
    }
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
