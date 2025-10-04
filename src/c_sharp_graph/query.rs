use std::{
    collections::{BTreeMap, HashMap, HashSet},
    vec,
};

use anyhow::{Error, Ok};
use regex::Regex;
use serde_json::Value;
use stack_graphs::{
    arena::Handle,
    graph::{Edge, File, Node, StackGraph},
};
use tracing::{debug, error, info, trace};
use url::Url;

use crate::c_sharp_graph::{
    loader::SourceType,
    method_query::MethodSymbolsGetter,
    namespace_query::NamespaceSymbolsGetter,
    results::{Location, Position, ResultNode},
};

pub trait Query {
    fn query(self, query: String) -> anyhow::Result<Vec<ResultNode>, Error>;
}

pub enum QueryType<'graph> {
    All {
        graph: &'graph StackGraph,
        source_type: &'graph SourceType,
    },
    Method {
        graph: &'graph StackGraph,
        source_type: &'graph SourceType,
    },
}

#[derive(Debug)]
pub enum SyntaxType {
    Import,
    CompUnit,
    NamespaceDeclaration,
    ClassDef,
    MethodName,
    LocalVar,
    Argument,
    Name,
}

impl SyntaxType {
    pub(crate) fn get(syntax_type_string: &str) -> Self {
        match syntax_type_string {
            "import" => Self::Import,
            "comp_unit" => Self::CompUnit,
            "namespace_declaration" => Self::NamespaceDeclaration,
            "class_def" => Self::ClassDef,
            "method_name" => Self::MethodName,
            "local_var" => Self::LocalVar,
            "argument" => Self::Argument,
            "name" => Self::Name,
            // Name is the least used thing, and I want to have a default for this.
            &_ => Self::Name,
        }
    }
}

#[derive(Eq, Hash, PartialEq, Debug)]
pub(crate) struct Fqdn {
    pub(crate) namespace: Option<String>,
    pub(crate) class: Option<String>,
    pub(crate) method: Option<String>,
}

pub(crate) fn get_fqdn(node: Handle<Node>, graph: &StackGraph) -> Option<Fqdn> {
    let mut fqdn = Fqdn {
        namespace: None,
        class: None,
        method: None,
    };
    // traverse upwards based on the FQDN edge
    // Once there is no FQDN edge, return
    let n = &graph[node];
    let source_info = graph
        .source_info(node)
        .expect("FQDN nodes must have source info");
    let syntax_type = &graph[source_info
        .syntax_type
        .into_option()
        .expect("FQDN nodes must have a syntax type")];
    let syntax_type = SyntaxType::get(syntax_type);
    // if this node that is from a FQDN does not have a symobl something is
    // very wrong in the TSG.
    let symbol_handle = n.symbol().unwrap();
    let symbol = graph[symbol_handle].to_string();
    let fqdn_edge = graph.outgoing_edges(node).find(|e| e.precedence == 10);
    match fqdn_edge {
        None => match syntax_type {
            SyntaxType::NamespaceDeclaration => {
                fqdn.namespace = Some(symbol);
                Some(fqdn)
            }
            SyntaxType::MethodName => {
                fqdn.method = Some(symbol);
                Some(fqdn)
            }
            SyntaxType::ClassDef => {
                fqdn.class = Some(symbol);
                Some(fqdn)
            }
            _ => None,
        },
        Some(e) => match get_fqdn(e.sink, graph) {
            None => Some(fqdn),
            Some(mut f) => match syntax_type {
                SyntaxType::NamespaceDeclaration => {
                    f.namespace = f.namespace.map_or_else(
                        || Some(symbol.clone()),
                        |n| Some(format!("{}.{}", n, symbol.clone())),
                    );
                    Some(f)
                }
                SyntaxType::MethodName => {
                    f.method = f.method.map_or_else(
                        || Some(symbol.clone()),
                        |m| Some(format!("{}.{}", m, symbol.clone())),
                    );
                    Some(f)
                }
                SyntaxType::ClassDef => {
                    f.class = f.class.map_or_else(
                        || Some(symbol.clone()),
                        |c| Some(format!("{}.{}", c, symbol.clone())),
                    );
                    Some(f)
                }
                _ => None,
            },
        },
    }
}

impl Query for QueryType<'_> {
    fn query(self, query: String) -> anyhow::Result<Vec<ResultNode>, Error> {
        match self {
            QueryType::All { graph, source_type } => {
                let q = Querier {
                    graph,
                    source_type,
                    _matcher_getter: NamespaceSymbolsGetter {},
                };
                q.query(query)
            }
            QueryType::Method { graph, source_type } => {
                info!("running method search");
                let q = Querier {
                    graph,
                    source_type,
                    _matcher_getter: MethodSymbolsGetter {},
                };
                q.query(query)
            }
        }
    }
}

pub(crate) struct Querier<'graph, T: GetMatcher> {
    pub(crate) graph: &'graph StackGraph,
    pub(crate) source_type: &'graph SourceType,
    _matcher_getter: T,
}

#[derive(Debug)]
pub(crate) struct StartingNodes {
    definition_root_nodes: Vec<Handle<Node>>,
    referenced_files: HashSet<Handle<File>>,
    file_to_compunit_handle: HashMap<Handle<File>, Handle<Node>>,
}

impl<'a, T: GetMatcher> Querier<'a, T> {
    pub(crate) fn get_search(&self, query: String) -> anyhow::Result<Search, Error> {
        Search::create_search(query)
    }

    pub(crate) fn get_starting_nodes(&self, search: &Search) -> StartingNodes {
        // get all the compilation units that use some portion of the search (using System or
        // using System.Configuration) This will require us to then determine if there qualified
        // names ConfigurationManager.AppSettings for examples;

        // We will also need to find the definition of this by looking at the namespace
        // declaration. then we need to capture all the nodes that are definitions attached to
        // this (for instance namespace System.Configuration; Class ConfigurationManager; method
        // AppSettings)
        let mut definition_root_nodes: Vec<Handle<Node>> = vec![];
        let mut referenced_files: HashSet<Handle<File>> = HashSet::new();
        let mut file_to_compunit_handle: HashMap<Handle<File>, Handle<Node>> = HashMap::new();

        for node_handle in self.graph.iter_nodes() {
            let node: &Node = &self.graph[node_handle];
            let file_handle = match node.file() {
                Some(h) => h,
                None => {
                    continue;
                }
            };
            let symbol_option = node.symbol();
            if symbol_option.is_none() {
                // If the node doesn't have a symbol to look at, then we should continue and it
                // only used to tie together other nodes.
                continue;
            }
            let symbol = &self.graph[node.symbol().unwrap()];
            let source_info = self.graph.source_info(node_handle);
            if source_info.is_none() {
                continue;
            }
            match source_info.unwrap().syntax_type.into_option() {
                None => continue,
                Some(handle) => {
                    let syntax_type = SyntaxType::get(&self.graph[handle]);
                    match syntax_type {
                        SyntaxType::CompUnit => {
                            file_to_compunit_handle.insert(file_handle, node_handle);
                        }
                        SyntaxType::Import => {
                            if search.partial_namespace(symbol) {
                                referenced_files.insert(file_handle);
                            }
                        }
                        SyntaxType::NamespaceDeclaration => {
                            if search.match_namespace(symbol) {
                                definition_root_nodes.push(node_handle);
                                referenced_files.insert(file_handle);
                            }
                        }
                        _ => continue,
                    }
                }
            }
        }

        StartingNodes {
            definition_root_nodes,
            referenced_files,
            file_to_compunit_handle,
        }
    }

    pub(crate) fn traverse_node_search(
        &self,
        node: Handle<Node>,
        symbol_matcher: &T::Matcher,
        results: &mut Vec<ResultNode>,
        file_uri: String,
    ) {
        let mut traverse_nodes: Vec<Handle<Node>> = vec![];
        for edge in self.graph.outgoing_edges(node) {
            if edge.precedence == 10 {
                continue;
            }
            traverse_nodes.push(edge.sink);
            let child_node = &self.graph[edge.sink];
            match child_node.symbol() {
                None => continue,
                Some(symbol_handle) => {
                    let symbol = &self.graph[symbol_handle];
                    if symbol_matcher.match_symbol(symbol.to_string()) {
                        let debug_node =
                            self.graph.node_debug_info(edge.sink).map_or(vec![], |d| {
                                d.iter()
                                    .map(|e| {
                                        let k = self.graph[e.key].to_string();
                                        let v = self.graph[e.value].to_string();
                                        (k, v)
                                    })
                                    .collect()
                            });

                        let edge_debug = self.graph.edge_debug_info(edge.source, edge.sink).map_or(
                            vec![],
                            |d| {
                                d.iter()
                                    .map(|e| {
                                        let k = self.graph[e.key].to_string();
                                        let v = self.graph[e.value].to_string();
                                        (k, v)
                                    })
                                    .collect()
                            },
                        );

                        let code_location: Location;
                        let line_number: usize;
                        match self.graph.source_info(edge.sink) {
                            None => {
                                continue;
                            }
                            Some(source_info) => {
                                line_number = source_info.span.start.line;
                                code_location = Location {
                                    start_position: Position {
                                        line: source_info.span.start.line,
                                        character: source_info.span.start.column.utf8_offset,
                                    },
                                    end_position: Position {
                                        line: source_info.span.end.line,
                                        character: source_info.span.end.column.utf8_offset,
                                    },
                                };
                                // source info is containing line is never saved or restored to the
                                // database.
                                //match source_info.containing_line.into_option() {
                                //   None => (),
                                //  Some(string_handle) => {
                                //     line = Some(self.db[string_handle].to_string());
                                //}
                                //}
                            }
                        }
                        let var: BTreeMap<String, Value> =
                            BTreeMap::from([("file".to_string(), Value::from(file_uri.clone()))]);
                        //if let Some(line) = line {
                        //   var.insert("line".to_string(), Value::from(line.trim()));
                        //}
                        trace!(
                            "found result for node: {:?} and edge: {:?}",
                            debug_node,
                            edge_debug
                        );
                        results.push(ResultNode {
                            file_uri: file_uri.clone(),
                            line_number,
                            code_location,
                            variables: var,
                        });
                    }
                }
            }
        }
        for n in traverse_nodes {
            self.traverse_node_search(n, symbol_matcher, results, file_uri.clone());
        }
    }
}

impl<'graph, T: GetMatcher> Query for Querier<'graph, T> {
    fn query(self, query: String) -> anyhow::Result<Vec<ResultNode>, Error> {
        let search: Search = self.get_search(query)?;

        debug!("search: {:?}", search);

        let mut results: Vec<ResultNode> = vec![];

        let starting_nodes = self.get_starting_nodes(&search);

        // Now that we have the all the nodes we need to build the reference symbols to match the *
        let symbol_matcher =
            T::get_matcher(self.graph, starting_nodes.definition_root_nodes, &search)?;

        let (is_source, symbol_handle) = match self.source_type {
            SourceType::Source { symbol_handle } => (true, Some(symbol_handle)),
            _ => (false, None),
        };
        for file in starting_nodes.referenced_files.iter() {
            let comp_unit_node_handle = match starting_nodes.file_to_compunit_handle.get(file) {
                Some(x) => x,
                None => {
                    debug!("unable to find compulation unit for file");
                    break;
                }
            };
            // This determines if the file is source code or not, but using the source_type symbol
            // graph node.
            if is_source
                && !self.graph.nodes_for_file(*file).any(|node_handle| {
                    let node = &self.graph[node_handle];

                    let symobl_handle = symbol_handle.unwrap();
                    if let Some(sh) = node.symbol() {
                        // This compares the source_type symbol handle to the nodes symbol
                        // as symbols are de-duplicated, this will check that the symbol for the
                        // given node is the one that we set for the source_type in the graph.
                        if sh.as_usize() == symobl_handle.as_usize() {
                            if self.source_type.get_string() != self.graph[sh] {
                                error!("SOMETHING IS VERY WRONG!!!!");
                            }
                            // We need to make sure that the compulation unit for the file is
                            // actually has an edge from teh source_type node.
                            let edges: Vec<Edge> = self.graph.outgoing_edges(node_handle).collect();
                            for edge in edges {
                                if edge.sink == *comp_unit_node_handle {
                                    return true;
                                }
                            }
                        }
                    }
                    false
                })
            {
                continue;
            }
            let f = &self.graph[*file];
            let file_url = Url::from_file_path(f.name());
            if file_url.is_err() {
                break;
            }
            let file_uri = file_url.unwrap().as_str().to_string();
            trace!("searching for matches in file: {}", f.name());
            self.traverse_node_search(
                *comp_unit_node_handle,
                &symbol_matcher,
                &mut results,
                file_uri,
            );
        }
        Ok(results)
    }
}

pub(crate) trait GetMatcher {
    type Matcher: SymbolMatcher;
    fn get_matcher(
        stack_graphs: &StackGraph,
        definition_root_nodes: Vec<Handle<Node>>,
        search: &Search,
    ) -> Result<Self::Matcher, Error>
    where
        Self: std::marker::Sized;
}

pub(crate) trait SymbolMatcher {
    fn match_symbol(&self, symbol: String) -> bool;
}

#[derive(Debug)]
struct SearchPart {
    part: String,
    regex: Option<Regex>,
}

#[derive(Debug)]
pub(crate) struct Search {
    parts: Vec<SearchPart>,
}

impl Search {
    fn create_search(query: String) -> anyhow::Result<Search, Error> {
        let mut parts: Vec<SearchPart> = vec![];
        let star_regex = Regex::new(".*")?;
        for part in query.split(".") {
            if part.contains("*") {
                let regex: Regex = if part == "*" {
                    star_regex.clone()
                } else {
                    let new_part = part.replace("*", "(.*)");
                    Regex::new(&new_part)?
                };

                parts.push(SearchPart {
                    part: part.to_string(),
                    regex: Some(regex),
                });
            } else {
                parts.push(SearchPart {
                    part: part.to_string(),
                    regex: None,
                })
            }
        }

        Ok(Search { parts })
    }
}

impl Search {
    pub(crate) fn partial_namespace(&self, symbol: &str) -> bool {
        // We will need to break apart the symbol based on "." then looping through, look at the
        // same index, and if it matches continue if it doesn't then return false.
        for (i, symbol_part) in symbol.split(".").enumerate() {
            if self.parts.len() <= i {
                break;
            }
            if !self.parts[i].matches(symbol_part) {
                return false;
            }
        }
        true
    }

    pub(crate) fn match_namespace(&self, symbol: &str) -> bool {
        for (i, symbol_part) in symbol.split(".").enumerate() {
            // Because we can assume that the last part here is a '*' right now,
            // we anything past that should match
            if self.parts.len() <= i {
                break;
            }
            if !self.parts[i].matches(symbol_part) {
                return false;
            }
        }
        true
    }

    pub(crate) fn match_symbol(&self, symbol: &str) -> bool {
        // If the parts list is empty this will panic, but that should never happen.
        let last_part = self.parts.last().unwrap();
        last_part.matches(symbol)
    }

    // fn import_match
    //Namespace Match
    //Part Match
    //Regex Match
    //???
}

impl SearchPart {
    fn matches(&self, match_string: &str) -> bool {
        match &self.regex {
            None => self.part == match_string,
            Some(r) => r.is_match(match_string),
        }
    }
}
