use anyhow::{anyhow, Error};
use prost_types::Struct;
use stack_graphs::graph::StackGraph;
use stack_graphs::serde::StackGraph as serialize_stack_graph;
use stack_graphs::stitching::ForwardCandidates;
use stack_graphs::storage::SQLiteReader;
use stack_graphs::NoCancellation;
use std::fmt::Debug;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::RwLock;
use tracing::debug;
use which::which;

use crate::c_sharp_graph::language_config::SourceNodeLanguageConfiguration;
use crate::c_sharp_graph::loader::{init_stack_graph, SourceType};
use crate::provider::dependency_resolution::Dependencies;

pub struct Project {
    pub location: PathBuf,
    pub db_path: PathBuf,
    pub dependencies: Arc<TokioMutex<Option<Vec<Dependencies>>>>,
    pub graph: Arc<Mutex<Option<StackGraph>>>,
    pub source_language_config: Arc<RwLock<Option<SourceNodeLanguageConfiguration>>>,
    pub analysis_mode: AnalysisMode,
    pub tools: Tools,
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

#[derive(Clone)]
pub struct Tools {
    pub ilspy_cmd: PathBuf,
    pub paket_cmd: PathBuf,
}

impl Project {
    const ILSPY_CMD_LOC_KEY: &str = "ilspy_cmd";
    const PAKET_CMD_LOC_KEY: &str = "paket_cmd";
    const ILSPY_CMD: &str = "ilspy";
    const PAKET_CMD: &str = "paket";
    pub fn new(
        location: PathBuf,
        db_path: PathBuf,
        analysis_mode: AnalysisMode,
        tools: Tools,
    ) -> Project {
        Project {
            location,
            db_path,
            dependencies: Arc::new(TokioMutex::new(None)),
            graph: Arc::new(Mutex::new(None)),
            source_language_config: Arc::new(RwLock::new(None)),
            analysis_mode,
            tools,
        }
    }

    pub fn get_tools(specific_provider_config: &Option<Struct>) -> Result<Tools, Error> {
        match specific_provider_config {
            Some(specific_provider_config) => {
                let ilspy_cmd = match specific_provider_config.fields.get(Self::ILSPY_CMD_LOC_KEY) {
                    Some(v) => match &v.kind {
                        Some(k) => match k {
                            prost_types::value::Kind::NullValue(_) => {
                                return Err(anyhow!("not valid ilspy_cmd"));
                            }
                            prost_types::value::Kind::NumberValue(_) => {
                                return Err(anyhow!("not valid ilspy_cmd"));
                            }
                            prost_types::value::Kind::StringValue(s) => PathBuf::from_str(s)?,
                            prost_types::value::Kind::BoolValue(_) => {
                                return Err(anyhow!("not valid ilspy_cmd"));
                            }
                            prost_types::value::Kind::StructValue(_) => {
                                return Err(anyhow!("not valid ilspy_cmd"));
                            }
                            prost_types::value::Kind::ListValue(_) => {
                                return Err(anyhow!("not valid ilspy_cmd"));
                            }
                        },
                        None => {
                            return Err(anyhow!("not valid ilspy_cmd"));
                        }
                    },
                    None => which(Self::ILSPY_CMD)?,
                };
                let paket_cmd = match specific_provider_config.fields.get(Self::PAKET_CMD_LOC_KEY) {
                    Some(v) => match &v.kind {
                        Some(k) => match k {
                            prost_types::value::Kind::NullValue(_) => {
                                return Err(anyhow!("not valid paket_cmd"));
                            }
                            prost_types::value::Kind::NumberValue(_) => {
                                return Err(anyhow!("not valid paket_cmd"));
                            }
                            prost_types::value::Kind::StringValue(s) => PathBuf::from_str(s)?,
                            prost_types::value::Kind::BoolValue(_) => {
                                return Err(anyhow!("not valid paket_cmd"));
                            }
                            prost_types::value::Kind::StructValue(_) => {
                                return Err(anyhow!("not valid paket_cmd"));
                            }
                            prost_types::value::Kind::ListValue(_) => {
                                return Err(anyhow!("not valid paket_cmd"));
                            }
                        },
                        None => {
                            return Err(anyhow!("not valid ilspy_cmd"));
                        }
                    },
                    None => which::which(Self::PAKET_CMD)?,
                };
                Ok(Tools {
                    ilspy_cmd,
                    paket_cmd,
                })
            }
            None => Ok(Tools {
                ilspy_cmd: which(Self::ILSPY_CMD)?,
                paket_cmd: which(Self::PAKET_CMD)?,
            }),
        }
    }

    pub async fn validate_language_configuration(self: &Arc<Self>) -> Result<(), Error> {
        let clone = self.clone();
        let lc = SourceNodeLanguageConfiguration::new(&tree_sitter_stack_graphs::NoCancellation)?;
        let mut lc_guard = clone.source_language_config.write().await;
        lc_guard.replace(lc);
        Ok(())
    }

    pub async fn get_project_graph(self: &Arc<Self>) -> Result<usize, Error> {
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

        let lc_guard = self.source_language_config.read().await;
        // If the databse is present we should consider use that and load into the graph
        let lc = lc_guard.as_ref().expect("unable to get read lock");
        let initialized_results = match init_stack_graph(
            &self.location,
            &self.db_path,
            &lc.source_type_node_info,
            &lc.language_config,
        ) {
            Ok(i) => i,
            Err(e) => return Err(anyhow!(e)),
        };

        if let Ok(mut graph_guard) = self.graph.lock() {
            graph_guard.replace(initialized_results.stack_graph);
        }
        Ok(initialized_results.files_loaded)
    }

    pub async fn get_source_type(self: &Arc<Self>) -> Option<Arc<SourceType>> {
        let clone = self.source_language_config.clone();
        let lc_guard = clone.read().await;

        match lc_guard.as_ref() {
            Some(x) => match self.analysis_mode {
                AnalysisMode::SourceOnly => Some(x.source_type_node_info.clone()),
                AnalysisMode::Full => Some(x.dependnecy_type_node_info.clone()),
            },
            None => None,
        }
    }
}
