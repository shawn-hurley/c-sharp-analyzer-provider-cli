use std::{
    collections::{BTreeMap, HashMap, HashSet},
    vec,
};

use crate::c_sharp_graph::{
    loader::SourceType,
    results::{Location, Position, ResultNode},
};
use anyhow::{Error, Ok};
use prost_types::Value;
use regex::Regex;
use stack_graphs::{
    arena::Handle,
    graph::{Edge, File, Node, StackGraph},
};
use tracing::{debug, field::debug, trace};
use url::Url;

pub struct Querier<'a> {
    db: &'a mut StackGraph,
    source_type: &'a SourceType,
}

pub trait Query {
    fn query(&mut self, query: String) -> anyhow::Result<Vec<ResultNode>, Error>;
}

impl<'a> Query for Querier<'a> {
    fn query(&mut self, query: String) -> anyhow::Result<Vec<ResultNode>, Error> {
        let search: Search = self.get_search(query)?;

        debug!("search: {:?}", search);

        let mut results: Vec<ResultNode> = vec![];

        // If we are search for all things from a ref
        // ex: System.Configuration.ConfigurationManager.* or System.Configuration.*
        // this means that we need to find the Nodes from the namespace, then find all the matches
        // for all the nodes in that namespace.
        if search.all_references_search() {
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
            let mut file_to_source_type_node: HashMap<Handle<File>, Handle<Node>> = HashMap::new();

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
                                debug!(
                                    "handling: node: {} symbol: {} with syntax_type: {:?}",
                                    node.display(self.db),
                                    &symbol,
                                    &syntax_type
                                );
                                if search.match_namespace(symbol) {
                                    definition_root_nodes.push(node_handle);
                                    referenced_files.insert(file_handle);
                                }
                                //TODO: Handle nested namespace declarations
                            }
                            &_ => continue,
                        }
                    }
                }
            }
            // Now that we have the all the nodes we need to build the reference symbols to match the *
            let namespace_symbols = NamespaceSymbols::new(self.db, definition_root_nodes)?;

            for file in referenced_files.iter() {
                let comp_unit_node_handle = match file_to_compunit_handle.get(file) {
                    Some(x) => x,
                    None => {
                        debug!("unable to find compulation unit for file");
                        break;
                    }
                };
                if let SourceType::Source {
                    symbol_handle: symobl_handle,
                } = self.source_type
                {
                    if let None = self.db.nodes_for_file(*file).find(|node_handle| {
                        let node = &self.db[*node_handle];
                        if let Some(sh) = node.symbol()
                            && sh.as_usize() == symobl_handle.as_usize()
                        {
                            let edges: Vec<Edge> = self.db.outgoing_edges(*node_handle).collect();
                            for edge in edges {
                                if edge.sink == *comp_unit_node_handle {
                                    return true;
                                }
                            }
                        }
                        false
                    }) {
                        continue;
                    }
                }
                let f = &self.db[*file];
                let file_url = Url::from_file_path(f.name());
                if file_url.is_err() {
                    break;
                }
                let file_uri = file_url.unwrap().as_str().to_string();
                self.traverse_node_search(
                    *comp_unit_node_handle,
                    &namespace_symbols,
                    &mut results,
                    file_uri,
                );
            }
        }
        Ok(results)
    }
}

impl<'a> Querier<'a> {
    pub fn get_query(db: &'a mut StackGraph, source_type: &'a SourceType) -> impl Query + use<'a> {
        Querier { db, source_type }
    }
    fn get_search(&self, query: String) -> anyhow::Result<Search, Error> {
        Search::create_search(query)
    }
    fn traverse_node_search(
        &mut self,
        node: Handle<Node>,
        namespace_symbols: &NamespaceSymbols,
        results: &mut Vec<ResultNode>,
        file_uri: String,
    ) {
        let mut traverse_nodes: Vec<Handle<Node>> = vec![];
        for edge in self.db.outgoing_edges(node) {
            traverse_nodes.push(edge.sink);
            let child_node = &self.db[edge.sink];
            match child_node.symbol() {
                None => continue,
                Some(symbol_handle) => {
                    let symbol = &self.db[symbol_handle];
                    if namespace_symbols.symbol_in_namespace(symbol.to_string()) {
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
                        let mut line: Option<String> = None;
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
                                match source_info.containing_line.into_option() {
                                    None => (),
                                    Some(string_handle) => {
                                        line = Some(self.db[string_handle].to_string());
                                    }
                                }
                            }
                        }
                        let mut var: BTreeMap<String, Value> =
                            BTreeMap::from([("file".to_string(), Value::from(file_uri.clone()))]);
                        if let Some(line) = line {
                            var.insert("line".to_string(), Value::from(line.as_str()));
                        }

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
            self.traverse_node_search(n, namespace_symbols, results, file_uri.clone());
        }
    }
}

pub struct NamespaceSymbols {
    classes: HashMap<String, Handle<Node>>,
    class_fields: HashMap<String, Handle<Node>>,
    class_methods: HashMap<String, Handle<Node>>,
}

impl NamespaceSymbols {
    fn new(
        db: &mut StackGraph,
        nodes: Vec<Handle<Node>>,
    ) -> anyhow::Result<NamespaceSymbols, Error> {
        let mut classes: HashMap<String, Handle<Node>> = HashMap::new();
        let mut class_fields: HashMap<String, Handle<Node>> = HashMap::new();
        let mut class_methods: HashMap<String, Handle<Node>> = HashMap::new();

        for node_handle in nodes {
            //Get all the edges
            Self::traverse_node(
                db,
                node_handle,
                &mut classes,
                &mut class_fields,
                &mut class_methods,
            )
        }

        Ok(NamespaceSymbols {
            classes,
            class_fields,
            class_methods,
        })
    }

    fn traverse_node(
        db: &mut StackGraph,
        node: Handle<Node>,
        classes: &mut HashMap<String, Handle<Node>>,
        _class_fields: &mut HashMap<String, Handle<Node>>,
        class_methods: &mut HashMap<String, Handle<Node>>,
    ) {
        let mut child_edges: Vec<Handle<Node>> = vec![];
        for edge in db.outgoing_edges(node) {
            child_edges.push(edge.sink);
            let child_node = &db[edge.sink];
            let symbol = match child_node.symbol() {
                None => continue,
                Some(symbol) => &db[symbol],
            };
            match db.source_info(edge.sink) {
                None => continue,
                Some(source_info) => match source_info.syntax_type.into_option() {
                    None => continue,
                    Some(syntax_type) => match &db[syntax_type] {
                        "method_name" => {
                            class_methods.insert(symbol.to_string(), edge.sink);
                        }
                        "class-def" => {
                            classes.insert(symbol.to_string(), edge.sink);
                        }
                        &_ => {}
                    },
                },
            }
        }
        for child_edge in child_edges {
            Self::traverse_node(db, child_edge, classes, _class_fields, class_methods);
        }
    }

    fn symbol_in_namespace(&self, symbol: String) -> bool {
        let class_match = self.classes.get(&symbol);
        let method_match = self.class_methods.get(&symbol);
        let field_match = self.class_fields.get(&symbol);

        if class_match.is_some() || method_match.is_some() || field_match.is_some() {
            return true;
        }
        false
    }
}

#[derive(Debug)]
struct SearchPart {
    part: String,
    regex: Option<Regex>,
}

#[derive(Debug)]
struct Search {
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
                    Regex::new(part)?
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

    fn all_references_search(&self) -> bool {
        let last = self.parts.last();
        match last {
            None => false,
            Some(part) => {
                if part.part == "*" {
                    return true;
                }
                false
            }
        }
    }

    fn partial_namespace(&self, symbol: &str) -> bool {
        // We will need to break apart the symbol based on "." then looping through, look at the
        // same index, and if it matches continue if it doesn't then return false.
        for (i, symbol_part) in symbol.split(".").enumerate() {
            if !self.parts[i].matches(symbol_part.to_string()) {
                return false;
            }
        }
        true
    }

    fn match_namespace(&self, symbol: &str) -> bool {
        let symbol_parts: Vec<&str> = symbol.split(".").collect();
        if symbol_parts.len() != self.parts.len() - 1 {
            return false;
        }
        for (i, symbol_part) in symbol_parts.iter().enumerate() {
            if !self.parts[i].matches(symbol_part.to_string()) {
                return false;
            }
        }
        true
    }

    // fn import_match
    //Namespace Match
    //Part Match
    //Regex Match
    //???
}

impl SearchPart {
    fn matches(&self, match_string: String) -> bool {
        match &self.regex {
            None => self.part == match_string,
            Some(r) => r.is_match(match_string.as_str()),
        }
    }
}
