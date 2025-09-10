use std::collections::BTreeMap;

#[derive(Debug)]
pub struct ResultNode {
    pub file_uri: String,
    pub line_number: usize,
    pub variables: BTreeMap<std::string::String, prost_types::Value>,
    pub code_location: Location,
}

#[derive(Debug)]
pub struct Position {
    pub line: usize,
    pub character: usize,
}

#[derive(Debug)]
pub struct Location {
    pub start_position: Position,
    pub end_position: Position,
}
