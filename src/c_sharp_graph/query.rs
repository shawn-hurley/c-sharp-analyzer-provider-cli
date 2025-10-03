use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt::format,
    vec,
};

use anyhow::{Error, Ok};
use regex::Regex;
use serde_json::Value;
use stack_graphs::{
    arena::Handle,
    graph::{Edge, File, Node, StackGraph, Symbol},
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
        db: &'graph StackGraph,
        source_type: &'graph SourceType,
    },
    Method {
        db: &'graph StackGraph,
        source_type: &'graph SourceType,
    },
}

#[derive(Eq, Hash, PartialEq, Debug)]
pub(crate) struct FQDN {
    pub(crate) namespace: Option<String>,
    pub(crate) class: Option<String>,
    pub(crate) method: Option<String>,
}

pub(crate) fn get_fqdn(node: Handle<Node>, graph: &StackGraph) -> Option<FQDN> {
    let mut fqdn = FQDN {
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
    // if this node that is from a FQDN does not have a symobl something is
    // very wrong in the TSG.
    let symbol_handle = n.symbol().unwrap();
    let symbol = graph[symbol_handle].to_string();
    let fqdn_edge = graph.outgoing_edges(node).find(|e| e.precedence == 10);
    match fqdn_edge {
        None => match syntax_type {
            "namespace-declaration" => {
                fqdn.namespace = Some(symbol);
                Some(fqdn)
            }
            "method_name" => {
                fqdn.method = Some(symbol);
                Some(fqdn)
            }
            "class-def" => {
                fqdn.class = Some(symbol);
                Some(fqdn)
            }
            _ => None,
        },
        Some(e) => match get_fqdn(e.sink, graph) {
            None => Some(fqdn),
            Some(mut f) => match syntax_type {
                "namespace-declaration" => {
                    f.namespace = f.namespace.map_or_else(
                        || Some(symbol.clone()),
                        |n| Some(format!("{}.{}", n, symbol.clone())),
                    );
                    Some(f)
                }
                "method_name" => {
                    f.method = f.method.map_or_else(
                        || Some(symbol.clone()),
                        |m| Some(format!("{}.{}", m, symbol.clone())),
                    );
                    Some(f)
                }
                "class-def" => {
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
            QueryType::All { db, source_type } => {
                let q = Querier {
                    db,
                    source_type,
                    _matcher_getter: NamespaceSymbolsGetter {},
                };
                q.query(query)
            }
            QueryType::Method { db, source_type } => {
                info!("running method search");
                let q = Querier {
                    db,
                    source_type,
                    _matcher_getter: MethodSymbolsGetter {},
                };
                q.query(query)
            }
        }
    }
}

pub(crate) struct Querier<'graph, T: GetMatcher> {
    pub(crate) db: &'graph StackGraph,
    pub(crate) source_type: &'graph SourceType,
    _matcher_getter: T,
}

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

        for node_handle in self.db.iter_nodes() {
            let node: &Node = &self.db[node_handle];
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
            let symbol = &self.db[node.symbol().unwrap()];
            let source_info = self.db.source_info(node_handle);
            if source_info.is_none() {
                continue;
            }
            match source_info.unwrap().syntax_type.into_option() {
                None => continue,
                Some(handle) => {
                    let syntax_type = &self.db[handle];
                    match syntax_type {
                        "comp-unit" => {
                            file_to_compunit_handle.insert(file_handle, node_handle);
                        }
                        "import" => {
                            if search.partial_namespace(symbol) {
                                referenced_files.insert(file_handle);
                            }
                        }
                        "namespace-declaration" => {
                            if search.match_namespace(symbol) {
                                definition_root_nodes.push(node_handle);
                                referenced_files.insert(file_handle);
                            }
                        }
                        &_ => continue,
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
        for edge in self.db.outgoing_edges(node) {
            if edge.precedence == 10 {
                debug!("edge precedence: {}", edge.precedence);
                continue;
            }
            traverse_nodes.push(edge.sink);
            let child_node = &self.db[edge.sink];
            match child_node.symbol() {
                None => continue,
                Some(symbol_handle) => {
                    let symbol = &self.db[symbol_handle];
                    if symbol_matcher.match_symbol(symbol.to_string()) {
                        let debug_node = self.db.node_debug_info(edge.sink).map_or(vec![], |d| {
                            d.iter()
                                .map(|e| {
                                    let k = self.db[e.key].to_string();
                                    let v = self.db[e.value].to_string();
                                    (k, v)
                                })
                                .collect()
                        });

                        let edge_debug =
                            self.db
                                .edge_debug_info(edge.source, edge.sink)
                                .map_or(vec![], |d| {
                                    d.iter()
                                        .map(|e| {
                                            let k = self.db[e.key].to_string();
                                            let v = self.db[e.value].to_string();
                                            (k, v)
                                        })
                                        .collect()
                                });

                        let code_location: Location;
                        let line_number: usize;
                        match self.db.source_info(edge.sink) {
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
            T::get_matcher(self.db, starting_nodes.definition_root_nodes, &search)?;

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
            if is_source
                && !self.db.nodes_for_file(*file).any(|node_handle| {
                    let node = &self.db[node_handle];

                    let symobl_handle = symbol_handle.unwrap();
                    if let Some(sh) = node.symbol() {
                        if sh.as_usize() == symobl_handle.as_usize() {
                            if self.source_type.get_string() != self.db[sh] {
                                error!("SOMETHING IS VERY WRONG!!!!");
                            }
                            let edges: Vec<Edge> = self.db.outgoing_edges(node_handle).collect();
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
            let f = &self.db[*file];
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
            // TODO: Consider how to make this handle any type of regex.
            //
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
