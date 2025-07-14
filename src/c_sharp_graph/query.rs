use std::{collections::{BTreeMap, HashMap, HashSet}, vec};

use anyhow::{Error, Ok};
use prost_types::Value;
use url::Url;
use regex::Regex;
use stack_graphs::{arena::Handle, graph::{File, Node, StackGraph}};
use crate::c_sharp_graph::results::{Location, Position, Result};

pub struct Querier<'a> {
    db: &'a mut StackGraph,
}

pub trait Query {
    fn query(&mut self, query: String) -> anyhow::Result<Vec<Result>, Error>;
}

impl Query for Querier<'_> {

    fn query(&mut self, query: String) -> anyhow::Result<Vec<Result>, Error> {
        let search: Search = self.get_search(query)?;

        println!("search: {:?}", search);

        let mut results: Vec<Result> = vec![];

        // If we are search for all things from a ref
        // ex: System.Configuration.ConfigurationManager.* or System.Configuration.*
        // this means that we need to find the Nodes from the namespace, then find all the matches
        // for all the nodes in that namespace.
        if search.all_references_search() {
            // get all the compilation units that use some portion of the search (using System or using System.Configuration)
            // This will require us to then determine if there qualified names ConfigurationManager.AppSettings for examples;

            // We will also need to find the definition of this by looking at the namepsace declartion. then we need to capture all the nodes that are
            // definitions attached to this (for instance namespace System.Configuration; Class ConfigurationManager; method AppSettings)
            let mut definition_root_nodes: Vec<Handle<Node>> = vec![];
            let mut referenced_files: HashSet<Handle<File>> = HashSet::new();
            let mut file_to_compunit_handle: HashMap<Handle<File>, Handle<Node>> = HashMap::new();

            for node_handle in self.db.iter_nodes() {
                println!("node_handle: {}", node_handle.display(self.db));
                let node: &Node = &self.db[node_handle];
                let symbol_option = node.symbol();
                if symbol_option.is_none() {
                    // If the node doesn't have a symbol to look at, then we should continue
                    // and it only used to tie together other nodes.
                    continue;
                }
                
                let symbol = &self.db[node.symbol().unwrap()];
                let source_info = self.db.source_info(node_handle);
                if source_info.is_none() {
                    println!("continue source_info: {}", node.display(self.db));
                    continue
                }
                match source_info.unwrap().syntax_type.into_option() {
                    None => continue,
                    Some(handle) => {
                        let syntax_type = &self.db[handle];
                        match syntax_type {
                            "comp-unit" => {
                                let filepath = node.file();
                                match filepath {
                                    None => continue,
                                    Some(file_handle) => {
                                        file_to_compunit_handle.insert(file_handle, node_handle);

                                    }
                                }
                            }
                            "import" => {
                                if search.partial_namespace(symbol) {
                                    let filepath = node.file();
                                    match filepath {
                                        None => continue,
                                        Some(file_handle) => {
                                            referenced_files.insert(file_handle);
                                        }
                                    }
                                }
                            }
                            "namespace-declaration" => {
                                if search.match_namespace(symbol) {
                                    definition_root_nodes.push(node_handle);
                                    match node.file() {
                                        None => continue,
                                        Some(file_handle) => {
                                            referenced_files.insert(file_handle);
                                        }
                                    }
                                }
                                //TODO: Handle nested namespace declarations
                            }
                            &_ => {
                                continue
                            }
                        }
                    }
                }
            }
            // Now that we have the all the nodes we need to build the reference symbols to match the *
            let namespace_symbols = NamespaceSymbols::new(self.db, definition_root_nodes)?;

            println!("referenced_files: {:?}", referenced_files);
            for file in referenced_files.iter() {
                let file_handle= file.to_owned();
                let comp_unit_node = file_to_compunit_handle.get(&file_handle);
                if comp_unit_node.is_none() {
                    println!("something went very wrong");
                    break;
                }
                let f = &self.db[file_handle];
                let file_url = Url::from_file_path(f.name());
                if file_url.is_err() {
                    println!("something went very wrong URI");
                    break;
                }
                let file_uri = file_url.unwrap().as_str().to_string();
                let _ = f;
                self.traverse_node_search(*comp_unit_node.unwrap(), &namespace_symbols, &mut results, file_uri);
            }

            println!("results - {:?}", results)
        }
        Ok(results)

    }
}

impl Querier<'_> {
    pub fn new(db: &mut StackGraph) -> impl Query + use<'_> {
        return Querier{db};
    }
    fn get_search(&self, query: String) -> anyhow::Result<Search, Error> {
       return Search::create_search(query);
    }
    fn traverse_node_search(&mut self, node: Handle<Node>, namespace_symbols: &NamespaceSymbols, results: &mut Vec<Result>, file_uri: String) {
        let mut traverse_nodes: Vec<Handle<Node>> = vec![];
        for edge in self.db.outgoing_edges(node) {
            traverse_nodes.push(edge.sink);
            let child_node = &self.db[edge.sink];
            match child_node.symbol() {
                None => {
                    continue
                },
                Some(symbol_handle) => {
                    let symbol = &self.db[symbol_handle];
                    if namespace_symbols.symbol_in_namespace(symbol.to_string()) {
                        let debug_ndoe = self.db.node_debug_info(edge.sink).map_or(vec![], |d| {
                            return d.iter().map(|e| {
                                let k = self.db[e.key].to_string();
                                let v = self.db[e.value].to_string();
                                return (k, v)
                            }).collect();
                        });
                        let edge_debug = self.db.edge_debug_info(edge.source, edge.sink).map_or(vec![], |d| {
                            return d.iter().map(|e| {
                                let k = self.db[e.key].to_string();
                                let v = self.db[e.value].to_string();
                                return (k, v)
                            }).collect();
                        });

                        

                        println!("{:?} -- {} - {:?} -- {:?}", child_node.scope(), child_node.display(self.db), debug_ndoe, edge_debug);
                        let code_location: Location;
                        let line_number: usize;
                        let mut line: Option<String> = None;
                        match self.db.source_info(edge.sink) {
                            None => {
                                println!("something is wrong this shouldn't happen");
                                continue;
                            },
                            Some(source_info) => {
                                line_number = source_info.span.start.line;
                                code_location = Location{
                                    start_position: Position{
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
                        let mut var: BTreeMap<String, Value> = BTreeMap::from([("file".to_string(), Value::from(file_uri.clone()))]);
                        println!("LINE -- {:?}", line);
                        if line.is_some() {
                            println!("here -- {:?}", line);
                            var.insert("line".to_string(), Value::from(line.unwrap().as_str()));
                        }
                        
                        results.push(Result{file_uri: file_uri.clone(), line_number, code_location, variables: var});
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
    fn new(db: &mut StackGraph, nodes: Vec<Handle<Node>>) -> anyhow::Result<NamespaceSymbols, Error> {
        let mut classes: HashMap<String, Handle<Node>> = HashMap::new();
        let mut class_fields: HashMap<String, Handle<Node>> = HashMap::new();
        let mut class_methods: HashMap<String, Handle<Node>> = HashMap::new();

        for node_handle in nodes {
            //Get all the edges
            Self::traverse_node(db, node_handle, &mut classes, &mut class_fields, &mut class_methods)
        }

        println!("classes- {:?}", classes);
        println!("class methods -{:?}", class_methods);
        println!("class fields - {:?}", class_fields);
        Ok(NamespaceSymbols {classes: classes, class_fields: class_fields, class_methods: class_methods })

    }

    fn traverse_node(db: &mut StackGraph, node: Handle<Node>, classes: &mut HashMap<String, Handle<Node>>, class_fields: &mut HashMap<String, Handle<Node>>, class_methods: &mut HashMap<String, Handle<Node>>) {
        let mut child_edges: Vec<Handle<Node>> = vec![];
        for edge in db.outgoing_edges(node) {
            child_edges.push(edge.sink);
            let child_node = &db[edge.sink];
            let symbol = match child_node.symbol() {
                None => continue,
                Some(symbol) => &db[symbol]
            };
            match db.source_info(edge.sink) {
                None => {
                    continue
                },
                Some(source_info) => {
                    match source_info.syntax_type.into_option() {
                        None => {
                            continue
                        },
                        Some(syntax_type) => {
                            match &db[syntax_type] {
                                "method_name" => {
                                    class_methods.insert(symbol.to_string(), edge.sink);
                                }
                                "class-def" => {
                                    classes.insert(symbol.to_string(), edge.sink);
                                }
                                &_ => {},
                            }
                        }
                    }
                }
            }

        }
        for child_edge in child_edges {
            Self::traverse_node(db, child_edge, classes, class_fields, class_methods);
        }
    }

    fn symbol_in_namespace(&self, symbol: String) -> bool {
        let class_match = self.classes.get(&symbol);
        let method_match = self.class_methods.get(&symbol);
        let field_match = self.class_fields.get(&symbol);

        if class_match.is_some() || method_match.is_some() || field_match.is_some() {
            return true
        }
        return false

    }
}


#[derive(Debug)]
struct SearchPart {
    part: String,
    regex: Option<Regex>
}

#[derive(Debug)]
struct Search {
    parts: Vec<SearchPart>,
}

impl Search {
    fn create_search(query: String) -> anyhow::Result<Search, Error> {
        let mut parts: Vec<SearchPart> = vec![];
        for part in query.split(".") {
            if part.contains("*") {
                let regex: Regex;
                if part == "*" {
                    regex = Regex::new(".*")?;
                } else {
                    regex = Regex::new(part)?;
                }

                parts.push(SearchPart{
                    part: part.to_string(),
                    regex: Some(regex),
                });
            } else {
                parts.push(SearchPart { part: part.to_string(), regex: None })
            }
        }

        return Ok(Search{parts: parts})
    }

    fn all_references_search(&self) -> bool {
        let last = self.parts.last();
        println!("last: {:?}", last);
        match last {
            None => {
                return false;
            }
            Some(part) => {
                if part.part == "*" {
                    return true;
                }
                return false;
            }
        }
    }

    fn partial_namespace(&self, symbol: &str) -> bool {
        // We will need to break apart the symbol based on "." then looping through, look at the same index, and if it matches continue
        // if it doesnt then return false.
        for (i, symbol_part) in symbol.split(".").enumerate() {
            if !self.parts[i].matches(symbol_part.to_string()) {
                return false;
            }
        }
        return true;
    }
    
    fn match_namespace(&self, symbol: &str) -> bool {
        let symbol_parts:Vec<&str> = symbol.split(".").collect();
        if symbol_parts.len() != self.parts.len()-1 {
            return false;
        }
        for (i, symbol_part) in symbol_parts.iter().enumerate() {
            if !self.parts[i].matches(symbol_part.to_string()) {
                return false
            }
        }
        return true;
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
            None => return self.part == match_string,
            Some(r) => return r.is_match(match_string.as_str()),
        }
    }
}