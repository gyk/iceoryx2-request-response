use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub enum Request {
    GetFileSize { path: PathBuf },
    GetFileContent { path: PathBuf },
}

#[derive(Serialize)]
pub enum Response {
    FileSize(u64),
    FileContent(Vec<u8>),
}

#[derive(Deserialize)]
pub enum ResponseRef<'a> {
    FileSize(u64),
    FileContent(&'a [u8]),
}
