use std::{collections::HashMap, vec};

use anyhow::{Error, Ok};
use stack_graphs::{
    arena::Handle,
    graph::{Node, StackGraph},
};

use crate::c_sharp_graph::query::{GetMatcher, Search, SymbolMatcher, SyntaxType};

pub(crate) struct NamespaceSymbolsGetter {}

impl GetMatcher for NamespaceSymbolsGetter {
    type Matcher = NamespaceSymbols;

    fn get_matcher(
        stack_graphs: &StackGraph,
        definition_root_nodes: Vec<Handle<Node>>,
        search: &Search,
    ) -> Result<Self::Matcher, Error>
    where
        Self: std::marker::Sized,
    {
        NamespaceSymbols::new(stack_graphs, definition_root_nodes, search)
    }
}

pub(crate) struct NamespaceSymbols {
    classes: HashMap<String, Handle<Node>>,
    class_fields: HashMap<String, Handle<Node>>,
    class_methods: HashMap<String, Handle<Node>>,
}

// Create exposed methods for NamesapceSymbols
impl NamespaceSymbols {
    pub(crate) fn new(
        graph: &StackGraph,
        nodes: Vec<Handle<Node>>,
        search: &Search,
    ) -> anyhow::Result<NamespaceSymbols, Error> {
        let mut classes: HashMap<String, Handle<Node>> = HashMap::new();
        let mut class_fields: HashMap<String, Handle<Node>> = HashMap::new();
        let mut class_methods: HashMap<String, Handle<Node>> = HashMap::new();

        for node_handle in nodes {
            //Get all the edges
            Self::traverse_node(
                graph,
                node_handle,
                search,
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
}

impl SymbolMatcher for NamespaceSymbols {
    fn match_symbol(&self, symbol: String) -> bool {
        self.symbol_in_namespace(symbol)
    }
}

// Private methods for NamespaceSymbols
impl NamespaceSymbols {
    fn traverse_node(
        db: &StackGraph,
        node: Handle<Node>,
        search: &Search,
        classes: &mut HashMap<String, Handle<Node>>,
        _class_fields: &mut HashMap<String, Handle<Node>>,
        class_methods: &mut HashMap<String, Handle<Node>>,
    ) {
        let mut child_edges: Vec<Handle<Node>> = vec![];
        for edge in db.outgoing_edges(node) {
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
                    Some(syntax_type) => match SyntaxType::get(&db[syntax_type]) {
                        SyntaxType::MethodName => {
                            class_methods.insert(symbol.to_string(), edge.sink);
                        }
                        SyntaxType::ClassDef => {
                            classes.insert(symbol.to_string(), edge.sink);
                        }
                        _ => {}
                    },
                },
            }
        }
        for child_edge in child_edges {
            Self::traverse_node(
                db,
                child_edge,
                search,
                classes,
                _class_fields,
                class_methods,
            );
        }
    }

    fn symbol_in_namespace(&self, symbol: String) -> bool {
        let class_match = self.classes.get(&symbol);
        let method_match = self.class_methods.get(&symbol);
        let field_match = self.class_fields.get(&symbol);

        class_match.is_some() || method_match.is_some() || field_match.is_some()
    }
}
