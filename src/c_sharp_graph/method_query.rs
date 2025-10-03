use std::{collections::HashMap, vec};

use anyhow::{Error, Ok};
use stack_graphs::{
    arena::Handle,
    graph::{self, Edge, Node, StackGraph},
};
use tracing::{debug, field::debug, trace};

use crate::c_sharp_graph::query::{get_fqdn, GetMatcher, Search, SymbolMatcher, FQDN};

pub(crate) struct MethodSymbolsGetter {}

impl GetMatcher for MethodSymbolsGetter {
    type Matcher = MethodSymbols;

    fn get_matcher(
        stack_graphs: &StackGraph,
        definition_root_nodes: Vec<Handle<Node>>,
        search: &Search,
    ) -> Result<Self::Matcher, Error>
    where
        Self: std::marker::Sized,
    {
        debug!("getting MethodSymbols matcher");
        MethodSymbols::new(stack_graphs, definition_root_nodes, search)
    }
}

pub(crate) struct MethodSymbols {
    methods: HashMap<FQDN, Handle<Node>>,
}

// Create exposed methods for NamesapceSymbols
impl MethodSymbols {
    pub(crate) fn new(
        db: &StackGraph,
        nodes: Vec<Handle<Node>>,
        search: &Search,
    ) -> anyhow::Result<MethodSymbols, Error> {
        let mut methods: HashMap<FQDN, Handle<Node>> = HashMap::new();

        for node_handle in nodes {
            //Get all the edges
            Self::traverse_node(db, node_handle, search, &mut methods)
        }

        debug!("method nodes found: {:?}", methods);

        Ok(MethodSymbols { methods })
    }
}

impl SymbolMatcher for MethodSymbols {
    fn match_symbol(&self, symbol: String) -> bool {
        self.symbol_in_namespace(symbol)
    }
}

// Private methods for NamespaceSymbols
impl MethodSymbols {
    fn traverse_node(
        db: &StackGraph,
        node: Handle<Node>,
        search: &Search,
        methods: &mut HashMap<FQDN, Handle<Node>>,
    ) {
        let mut child_edges: Vec<Handle<Node>> = vec![];
        for edge in db.outgoing_edges(node) {
            debug!("edge precedence during search: {}", edge.precedence);
            if edge.precedence == 10 {
                continue;
            }
            child_edges.push(edge.sink);
            let child_node = &db[edge.sink];
            let symbol = match child_node.symbol() {
                None => continue,
                Some(symbol) => &db[symbol],
            };
            if !search.match_symbol(symbol) {
                continue;
            }
            match db.source_info(edge.sink) {
                None => continue,
                Some(source_info) => match source_info.syntax_type.into_option() {
                    None => continue,
                    Some(syntax_type) => {
                        if &db[syntax_type] == "method_name" {
                            let fqdn_name = get_fqdn(edge.sink, db)
                                .expect("We should always get a FQDN for methods");
                            methods.insert(fqdn_name, node);
                        }
                    }
                },
            }
        }
        for child_edge in child_edges {
            Self::traverse_node(db, child_edge, search, methods);
        }
    }

    // Symbol here must be of <thing>.<method_name>.
    // <thing> may be a class or a variable.
    // if a variable, we may have to enhance this method
    // to get the actual "class" of the variable.
    // TODO: Consider scoped things for this(??)
    // TODO: Consider a edge from the var to the class symbol
    fn symbol_in_namespace(&self, symbol: String) -> bool {
        trace!("checking symbol: {}", symbol);
        let parts: Vec<&str> = symbol.split(".").collect();
        if parts.len() != 2 {
            return false;
        }
        let method_part = parts
            .last()
            .expect("unable to get method part for symbol")
            .to_string();
        let class_part = parts
            .first()
            .expect("unable to get class part for symbol")
            .to_string();
        self.methods.keys().any(|fqdn| {
            let method = fqdn.method.clone().unwrap_or("".to_string());
            let class = fqdn.class.clone().unwrap_or("".to_string());
            method == method_part && class == class_part
        })
    }
}
