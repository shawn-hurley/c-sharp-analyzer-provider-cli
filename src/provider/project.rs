use anyhow::{anyhow, Error};
use stack_graphs::graph::Edge;
use stack_graphs::graph::StackGraph;
use stack_graphs::serde::StackGraph as serialize_stack_graph;
use stack_graphs::stitching::ForwardCandidates;
use stack_graphs::storage::SQLiteReader;
use stack_graphs::NoCancellation;
use std::fmt::Debug;
use std::ops::DerefMut;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use tracing::debug;
use tracing::error;

use crate::c_sharp_graph::language_config::SourceNodeLanguageConfiguration;
use crate::c_sharp_graph::loader::{init_stack_graph, SourceType};
use crate::provider::dependency_resolution::Dependencies;

pub struct Project {
    pub location: PathBuf,
    pub db_path: PathBuf,
    pub dependencies: Arc<Mutex<Option<Vec<Dependencies>>>>,
    pub graph: Arc<Mutex<Option<StackGraph>>>,
    pub source_language_config: Arc<Mutex<Option<SourceNodeLanguageConfiguration>>>,
    pub analysis_mode: AnalysisMode,
}

#[derive(Eq, PartialEq, Debug)]
pub enum AnalysisMode {
    Full,
    SourceOnly,
}

impl From<&str> for AnalysisMode {
    fn from(value: &str) -> Self {
        match value {
            "full" => AnalysisMode::Full,
            "source-only" => AnalysisMode::SourceOnly,
            _ => AnalysisMode::Full,
        }
    }
}
impl From<&String> for AnalysisMode {
    fn from(value: &String) -> Self {
        match value.as_str() {
            "full" => AnalysisMode::Full,
            "source-only" => AnalysisMode::SourceOnly,
            _ => AnalysisMode::Full,
        }
    }
}
impl From<String> for AnalysisMode {
    fn from(value: String) -> Self {
        match value.as_str() {
            "full" => AnalysisMode::Full,
            "source-only" => AnalysisMode::SourceOnly,
            _ => AnalysisMode::Full,
        }
    }
}

impl Debug for Project {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Project")
            .field("location", &self.location)
            .field("db_path", &self.db_path)
            .field("dependencies", &self.dependencies)
            .field("analysis_mode", &self.analysis_mode)
            .finish()
    }
}

impl Project {
    pub fn new(location: PathBuf, db_path: PathBuf, analysis_mode: AnalysisMode) -> Project {
        Project {
            location,
            db_path,
            dependencies: Arc::new(Mutex::new(None)),
            graph: Arc::new(Mutex::new(None)),
            source_language_config: Arc::new(Mutex::new(None)),
            analysis_mode,
        }
    }

    pub fn validate_language_configuration(self: &Arc<Self>) -> Result<(), Error> {
        let clone = self.clone();
        let lc = SourceNodeLanguageConfiguration::new(&tree_sitter_stack_graphs::NoCancellation)?;
        match clone.source_language_config.lock() {
            Ok(mut lc_guard) => {
                lc_guard.replace(lc);
                Ok(())
            }
            Err(e) => {
                error!("unable to get project source language config: {}", e);
                Err(anyhow!("unable to get project source config"))
            }
        }
    }

    pub fn get_project_graph(self: &Arc<Self>) -> Result<usize, Error> {
        // TODO: Handle database already exists
        if self.db_path.exists() {
            debug!("trying to load from existing db: {:?}", &self.db_path);
            // Load the stack_graph.
            let mut db_reader = match SQLiteReader::open(&self.db_path) {
                Ok(db_reader) => db_reader,
                Err(e) => {
                    return Err(anyhow!(e));
                }
            };
            debug!("got db reader");

            if let Err(e) =
                db_reader.load_graphs_for_file_or_directory(&self.location, &NoCancellation)
            {
                return Err(anyhow!(e));
            }
            debug!("loaded_files");

            let (stack_graph, _, _) = db_reader.get_graph_partials_and_db();
            debug!(
                "got stack graph from db with file: {}",
                stack_graph.iter_files().count()
            );
            debug!("starting serialize_stack_graph");
            let serialize_stack_graph = serialize_stack_graph::from_graph(stack_graph);
            let mut graph = StackGraph::new();
            debug!("loading graph");
            if let Err(e) = serialize_stack_graph.load_into(&mut graph) {
                debug!("unable to load graph: {}", e);
            }
            debug!("finish loading graph");
            if let Ok(mut lc_guard) = self.source_language_config.lock()
                && let Some(lc) = lc_guard.deref_mut()
            {
                graph.iter_nodes().for_each(|n| {
                    let node = &graph[n];
                    if let Some(symbol) = node.symbol() {
                        if symbol == lc.source_type_node_info.get_symbol_handle()
                            || symbol == lc.dependnecy_type_node_info.get_symbol_handle()
                        {
                            let edges: Vec<Edge> = graph.outgoing_edges(n).collect();
                            debug!("edges: {:?} for node: {}", edges, node.display(&graph))
                        }
                    }
                });
            }
            if graph.iter_symbols().count() == 0 {
                debug!("unable to load graph");
            } else {
                debug!("trying to get guard");
                if let Ok(mut graph_guard) = self.graph.lock() {
                    graph_guard.replace(graph);
                    drop(graph_guard);
                    debug!("setting graph on project");
                    return Ok(stack_graph.iter_files().count());
                }
            }
            drop(graph);
        }

        if let Ok(mut lc_guard) = self.source_language_config.lock()
            && let Some(lc) = lc_guard.deref_mut()
        {
            // If the databse is present we should consider use that and load into the graph
            let initialized_results = match init_stack_graph(
                &self.location,
                &self.db_path,
                &lc.source_type_node_info,
                &mut lc.loader,
            ) {
                Ok(i) => i,
                Err(e) => return Err(anyhow!(e)),
            };
            initialized_results.stack_graph.iter_nodes().for_each(|n| {
                let node = &initialized_results.stack_graph[n];
                if let Some(symbol) = node.symbol() {
                    if symbol == lc.source_type_node_info.get_symbol_handle()
                        || symbol == lc.dependnecy_type_node_info.get_symbol_handle()
                    {
                        let edges: Vec<Edge> =
                            initialized_results.stack_graph.outgoing_edges(n).collect();
                        debug!(
                            "edges: {:?} for node: {}",
                            edges,
                            node.display(&initialized_results.stack_graph)
                        )
                    }
                }
            });

            if let Ok(mut graph_guard) = self.graph.lock() {
                graph_guard.replace(initialized_results.stack_graph);
            }
            return Ok(initialized_results.files_loaded);
        };
        Err(anyhow!("unable to get project graph"))
    }

    pub fn get_source_type(self: &Arc<Self>) -> Option<Arc<SourceType>> {
        let clone = self.source_language_config.clone();
        let lc_guard = clone.lock().expect("unable to get source language config");

        match lc_guard.as_ref() {
            Some(x) => match self.analysis_mode {
                AnalysisMode::SourceOnly => Some(x.source_type_node_info.clone()),
                AnalysisMode::Full => Some(x.dependnecy_type_node_info.clone()),
            },
            None => None,
        }
    }
}
