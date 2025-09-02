use crate::c_sharp_graph::try_language_configuration;
use anyhow::{Error, Result};
use base64::Engine;
use sha1::{Digest, Sha1};
use stack_graphs::{
    graph::StackGraph,
    partial::{PartialPath, PartialPaths},
    storage::SQLiteWriter,
};
use std::path::{Path, PathBuf};
use tracing::error;
use tree_sitter_stack_graphs::{
    loader::{FileReader, Loader},
    NoCancellation, Variables, FILE_PATH_VAR, ROOT_PATH_VAR,
};
use walkdir::WalkDir;

#[derive(Debug)]
pub struct Stats {
    pub files_loaded: usize,
}
pub fn load_database(source_location: &Path, db_path: PathBuf) -> Result<Stats, Error> {
    let mut db: SQLiteWriter = SQLiteWriter::open(db_path.as_path())?;

    let lc = try_language_configuration(&NoCancellation).map_err(|err| Error::new(err))?;

    // If the db is already populated at the location specified, then we should return as already populated.
    let mut loader = Loader::from_language_configurations(vec![lc], None).map_err(Error::new)?;

    let mut stats = Stats { files_loaded: 0 };
    for path in WalkDir::new(source_location).into_iter() {
        let entry = match path {
            Ok(entry) => {
                if entry.file_type().is_dir() {
                    continue;
                }
                entry
            }
            Err(err) => return Err(Error::new(err)),
        };

        stats.files_loaded += 1;
        let mut file_reader = FileReader::new();
        let lcs = match loader.load_for_file(entry.path(), &mut file_reader, &NoCancellation) {
            Ok(lcs) => {
                if lcs.primary.is_some() {
                    lcs.primary.unwrap()
                } else {
                    stats.files_loaded -= 1;
                    continue;
                }
            }
            Err(err) => return Err(Error::new(err)),
        };
        let source = file_reader.get(entry.path())?;
        let tag: String = sha1(source);

        let mut globals = Variables::new();
        globals
            .add(
                FILE_PATH_VAR.into(),
                entry.to_owned().into_path().to_str().unwrap().into(),
            )
            .expect("failed to add file path variable");

        globals
            .add(
                ROOT_PATH_VAR.into(),
                entry
                    .to_owned()
                    .into_path()
                    .parent()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .into(),
            )
            .expect("failed to add root path variable");

        let mut graph = StackGraph::new();
        let file = match graph.add_file(entry.to_owned().into_path().to_str().unwrap()) {
            Ok(handle) => handle,
            Err(handle) => handle,
        };
        let build_result =
            lcs.sgl
                .build_stack_graph_into(&mut graph, file, source, &globals, &NoCancellation);
        if build_result.is_err() {
            error!(
                "unable to build graph for {:?}: {:?}",
                entry,
                build_result.err()
            );
            stats.files_loaded -= 1;
        }

        let mut partials = PartialPaths::new();
        let paths: Vec<PartialPath> = Vec::new();

        match db.store_result_for_file(&graph, file, &tag, &mut partials, &paths) {
            Ok(_) => continue,
            Err(err) => return Err(Error::new(err)),
        }
    }

    Ok(stats)
}

fn sha1(source: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(source);
    base64::prelude::BASE64_STANDARD_NO_PAD.encode(hasher.finalize())
}
