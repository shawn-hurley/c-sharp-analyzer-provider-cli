use anyhow::{anyhow, Error, Result};
use base64::Engine;
use sha1::{Digest, Sha1};
use stack_graphs::{
    arena::Handle,
    graph::{File, Node, NodeID, StackGraph, Symbol},
    partial::{PartialPath, PartialPaths},
    storage::SQLiteWriter,
};
use std::path::{Path, PathBuf};
use tracing::{debug, error, trace};
use tree_sitter_stack_graphs::{
    loader::{FileReader, Loader},
    NoCancellation, Variables, FILE_PATH_VAR, ROOT_PATH_VAR,
};
use walkdir::WalkDir;

pub const SOURCE_TYPE_NODE: &str = "SOURCE_TYPE_NODE";

#[derive(Debug, PartialEq, Eq, Hash)]
pub enum SourceType {
    Source { symbol_handle: Handle<Symbol> },
    Dependency { symbol_handle: Handle<Symbol> },
}

impl SourceType {
    pub fn get_source_string() -> String {
        "konveyor.io/source_type=source".to_string()
    }

    pub fn get_dependency_string() -> String {
        "konveyor.io/source_type=dependency".to_string()
    }

    pub fn get_symbol_handle(&self) -> Handle<Symbol> {
        match self {
            SourceType::Source { symbol_handle } | SourceType::Dependency { symbol_handle } => {
                *symbol_handle
            }
        }
    }

    pub fn load_node_to_graph(
        &self,
        graph: &mut StackGraph,
        file: Handle<File>,
    ) -> Result<NodeID, Error> {
        let symbol_handle = self.get_symbol_handle();
        let node_id = graph.new_node_id(file);
        match graph.add_pop_symbol_node(node_id, symbol_handle, false) {
            Some(_) => {
                trace!("added source type node to file")
            }
            None => {
                return Err(anyhow!("unable to add node to file"));
            }
        };
        Ok(node_id)
    }
}

pub struct InitializedGraph {
    pub files_loaded: usize,
    pub stack_graph: StackGraph,
}

pub fn add_dir_to_graph(
    source_location: &Path,
    db_path: &Path,
    source_type: &SourceType,
    loader: &mut Loader,
    original_graph: StackGraph,
) -> Result<InitializedGraph, Error> {
    let mut db: SQLiteWriter = SQLiteWriter::open(db_path)?;

    let mut stack_graph = original_graph;
    let mut files_loaded = 0;
    for path in WalkDir::new(source_location).into_iter() {
        debug!(
            "stack_graph files: {}, nodes: {}, symbols: {}",
            stack_graph.iter_files().count(),
            stack_graph.iter_nodes().count(),
            stack_graph.iter_symbols().count()
        );
        let entry = match path {
            Ok(entry) => {
                if entry.file_type().is_dir() {
                    continue;
                }
                entry
            }
            Err(err) => return Err(Error::new(err)),
        };
        let entry_path = entry.to_owned().into_path();
        let entry_path_str = match entry_path.to_str() {
            Some(path) => path,
            None => {
                return Err(anyhow!("unable to get path string"));
            }
        };
        if let Some(file_handle) = &stack_graph.get_file(entry_path_str) {
            debug!(
                "already added file to graph: {:?} - handle: {:?}",
                &entry_path, file_handle
            );
            continue;
        }
        match load_graph_for_file(
            entry_path.clone(),
            &mut stack_graph,
            loader,
            &mut db,
            source_type,
        ) {
            Ok(res) => match res {
                Some(f) => {
                    files_loaded += 1;
                    debug!("loaded file handle: {:?} - file: {:?}", f, &entry_path)
                }
                None => {
                    debug!("skipped file: {:?}", entry_path);
                }
            },
            Err(e) => {
                return Err(anyhow!("unable to load file: {:?} - {}", entry_path, e));
            }
        }
    }
    Ok(InitializedGraph {
        files_loaded,
        stack_graph,
    })
}

fn load_graph_for_file(
    entry: PathBuf,
    stack_graph: &mut StackGraph,
    loader: &mut Loader,
    db: &mut SQLiteWriter,
    source_type: &SourceType,
) -> Result<Option<Handle<File>>, Error> {
    let mut file_reader = FileReader::new();
    debug!("loading file: {:?}", entry);
    let entry_parent = entry.parent().expect("parent path should be available");

    let lc = match loader
        .load_for_file(&entry, &mut file_reader, &NoCancellation)?
        .primary
    {
        Some(x) => x,
        None => {
            return Ok(None);
        }
    };

    let source = file_reader.get(&entry)?;
    let tag: String = sha1(source);

    let mut globals = Variables::new();
    globals
        .add(
            FILE_PATH_VAR.into(),
            entry.to_str().expect("path to string").into(),
        )
        .expect("failed to add file path variable");

    globals
        .add(
            ROOT_PATH_VAR.into(),
            entry_parent.to_str().expect("to string").into(),
        )
        .expect("failed to add root path variable");

    let res = stack_graph.add_from_graph(&lc.builtins);
    if let Err(e) = res {
        error!("builtin error: {:?}", e);
    }
    let file = match stack_graph.add_file(&entry.to_str().unwrap()) {
        Ok(x) => x,
        Err(_) => {
            debug!("this found: {:?}", entry);
            return Err(anyhow!("unable to add file to graph"));
        }
    };
    let source_type_node_id = match source_type.load_node_to_graph(stack_graph, file) {
        Ok(id) => id,
        Err(e) => {
            return Err(anyhow!(e));
        }
    };
    let mut builder = lc.sgl.builder_into_stack_graph(stack_graph, file, source);
    let graph_node = builder.inject_node(source_type_node_id);
    globals
        .add(SOURCE_TYPE_NODE.into(), graph_node.into())
        .expect("adding source type node");

    let build_result = builder.build(&globals, &NoCancellation);
    if let Err(e) = build_result {
        error!("unable to build graph for {:?}: {:?}", entry, e);
        return Err(anyhow!("unable to build graph"));
    }

    let mut partials = PartialPaths::new();
    let paths: Vec<PartialPath> = Vec::new();

    match db.store_result_for_file(stack_graph, file, &tag, &mut partials, &paths) {
        Ok(_) => Ok(Some(file)),
        Err(err) => {
            error!("error: {}", err);
            Err(anyhow!(err))
        }
    }
}

pub fn init_stack_graph(
    source_location: &Path,
    db_path: &Path,
    source_type: &SourceType,
    loader: &mut Loader,
) -> Result<InitializedGraph, Error> {
    let mut db: SQLiteWriter = SQLiteWriter::open(db_path)?;

    let mut files_loaded = 0;

    let mut stack_graph = StackGraph::new();
    for path in WalkDir::new(source_location).into_iter() {
        debug!(
            "stack_graph files: {}, nodes: {}, symbols: {}",
            stack_graph.iter_files().count(),
            stack_graph.iter_nodes().count(),
            stack_graph.iter_symbols().count()
        );
        let entry = match path {
            Ok(entry) => {
                if entry.file_type().is_dir() {
                    continue;
                }
                entry
            }
            Err(err) => return Err(Error::new(err)),
        };
        let entry_path = entry.to_owned().into_path();
        match load_graph_for_file(
            entry_path.clone(),
            &mut stack_graph,
            loader,
            &mut db,
            source_type,
        ) {
            Ok(res) => match res {
                Some(f) => {
                    files_loaded += 1;
                    debug!("loaded file handle: {:?} - file: {:?}", f, entry_path)
                }
                None => debug!("skipped file: {:?}", entry_path),
            },
            Err(e) => {
                return Err(anyhow!("unable to load file: {:?} - {}", entry_path, e));
            }
        }
    }

    Ok(InitializedGraph {
        files_loaded,
        stack_graph,
    })
}

fn sha1(source: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(source);
    base64::prelude::BASE64_STANDARD_NO_PAD.encode(hasher.finalize())
}
