#![allow(dead_code)]
use std::borrow::Cow;
use std::sync::Arc;

use anyhow::Error;
use stack_graphs::graph::NodeID;
use stack_graphs::graph::StackGraph;
use tracing::debug;
use tree_sitter_graph::Variables;
use tree_sitter_stack_graphs::loader::FileAnalyzers;
use tree_sitter_stack_graphs::loader::LanguageConfiguration;
use tree_sitter_stack_graphs::loader::LoadError;
use tree_sitter_stack_graphs::loader::Loader;
use tree_sitter_stack_graphs::CancellationFlag;
use tree_sitter_stack_graphs::StackGraphLanguage;
use tree_sitter_stack_graphs::FILE_PATH_VAR;

use crate::c_sharp_graph::loader::SourceType;
use crate::c_sharp_graph::loader::SOURCE_TYPE_NODE;

pub const STACK_GRAPHS_TSG_PATH: &str = "src/stack-graphs.tsg";
/// The stack graphs tsg source for this language.
pub const STACK_GRAPHS_TSG_SOURCE: &str = include_str!("stack-graphs.tsg");

/// The stack graphs builtins configuration for this language.
pub const STACK_GRAPHS_BUILTINS_CONFIG: &str = include_str!("builtins.cfg");
/// The stack graphs builtins path for this language
pub const STACK_GRAPHS_BUILTINS_PATH: &str = "src/builtins.cs";
/// The stack graphs builtins source for this language.
pub const STACK_GRAPHS_BUILTINS_SOURCE: &str = include_str!("builtins.cs");

const BUILTINS_FILENAME: &str = "<builtins>";

pub struct SourceNodeLanguageConfiguration {
    pub loader: Loader,
    pub source_type_node_info: Arc<SourceType>,
    pub dependnecy_type_node_info: Arc<SourceType>,
}

impl SourceNodeLanguageConfiguration {
    pub fn new(
        cancellation_flag: &dyn CancellationFlag,
    ) -> Result<SourceNodeLanguageConfiguration, Error> {
        debug!("here get language config");
        let sgl = StackGraphLanguage::from_source(
            tree_sitter_c_sharp::LANGUAGE.into(),
            STACK_GRAPHS_TSG_PATH.into(),
            STACK_GRAPHS_TSG_SOURCE,
        )
        .map_err(|err| LoadError::SglParse {
            inner: err,
            tsg_path: STACK_GRAPHS_TSG_PATH.into(),
            tsg: Cow::from(STACK_GRAPHS_TSG_SOURCE),
        })?;
        let mut builtins = StackGraph::new();
        let mut builtins_globals = Variables::new();

        Loader::load_globals_from_config_str(STACK_GRAPHS_BUILTINS_CONFIG, &mut builtins_globals)?;

        builtins_globals
            .add(FILE_PATH_VAR.into(), BUILTINS_FILENAME.into())
            .unwrap_or_default();

        let file = builtins.add_file(BUILTINS_FILENAME).unwrap();
        let source_type_symbol_handle = builtins.add_symbol(&SourceType::get_source_string());
        let dependency_type_symbol_handle =
            builtins.add_symbol(&SourceType::get_dependency_string());
        let dependnecy_type_node_info = SourceType::Dependency {
            symbol_handle: dependency_type_symbol_handle,
        };
        let source_type_node_info = SourceType::Source {
            symbol_handle: source_type_symbol_handle,
        };
        let source_type_node_id = source_type_node_info.load_node_to_graph(&mut builtins, file)?;
        let dependency_type_node_id =
            dependnecy_type_node_info.load_node_to_graph(&mut builtins, file)?;
        let _ = match builtins.add_pop_symbol_node(
            source_type_node_id,
            source_type_symbol_handle,
            false,
        ) {
            Some(x) => x,
            None => builtins
                .node_for_id(source_type_node_id)
                .expect("could not get dependency node"),
        };
        let _ = match builtins.add_pop_symbol_node(
            dependency_type_node_id,
            dependency_type_symbol_handle,
            false,
        ) {
            Some(x) => x,
            None => builtins
                .node_for_id(dependency_type_node_id)
                .expect("could not get dependency node"),
        };

        let mut builder =
            sgl.builder_into_stack_graph(&mut builtins, file, STACK_GRAPHS_BUILTINS_SOURCE);
        let graph_node =
            builder.inject_node(NodeID::new_in_file(file, source_type_node_id.local_id()));
        debug!("graph_node_ref: {}", graph_node);
        match builtins_globals.get(&SOURCE_TYPE_NODE.into()) {
            Some(_) => {
                builtins_globals.remove(&SOURCE_TYPE_NODE.into());
                builtins_globals
                    .add(SOURCE_TYPE_NODE.into(), graph_node.into())
                    .unwrap_or_default();
            }
            None => {
                builtins_globals
                    .add(SOURCE_TYPE_NODE.into(), graph_node.into())
                    .unwrap_or_default();
            }
        };

        sgl.build_stack_graph_into(
            &mut builtins,
            file,
            STACK_GRAPHS_BUILTINS_SOURCE,
            &builtins_globals,
            cancellation_flag,
        )
        .map_err(|err| LoadError::Builtins {
            inner: err,
            source_path: STACK_GRAPHS_BUILTINS_PATH.into(),
            source: Cow::from(STACK_GRAPHS_BUILTINS_SOURCE),
            tsg_path: sgl.tsg_path().to_path_buf(),
            tsg: Cow::from(STACK_GRAPHS_TSG_SOURCE),
        })?;
        let lc = LanguageConfiguration {
            language: tree_sitter_c_sharp::LANGUAGE.into(),
            scope: Some("source.cs".to_string()),
            content_regex: None,
            file_types: vec![String::from("cs")],
            sgl,
            builtins,
            special_files: FileAnalyzers::new(),
            no_similar_paths_in_file: false,
        };
        let loader = Loader::from_language_configurations(vec![lc], None)?;
        Ok(SourceNodeLanguageConfiguration {
            loader,
            source_type_node_info: Arc::new(source_type_node_info),
            dependnecy_type_node_info: Arc::new(dependnecy_type_node_info),
        })
    }
}
