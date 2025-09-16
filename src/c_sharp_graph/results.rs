use std::collections::BTreeMap;

use crate::analyzer_service::{
    IncidentContext, Location as ProtoLocation, Position as ProtoPosition,
};
use prost_types::Struct;

#[derive(Debug)]
pub struct ResultNode {
    pub file_uri: String,
    pub line_number: usize,
    pub variables: BTreeMap<std::string::String, prost_types::Value>,
    pub code_location: Location,
}

impl From<ResultNode> for IncidentContext {
    fn from(val: ResultNode) -> Self {
        IncidentContext {
            file_uri: val.file_uri.clone(),
            effort: None,
            code_location: Some(val.code_location.into()),
            line_number: Some(val.line_number as i64),
            variables: Some(Struct {
                fields: val.variables.clone(),
            }),
            links: vec![],
            is_dependency_incident: false,
        }
    }
}

#[derive(Debug)]
pub struct Position {
    pub line: usize,
    pub character: usize,
}

impl From<Position> for ProtoPosition {
    fn from(val: Position) -> Self {
        ProtoPosition {
            line: val.line as f64,
            character: val.character as f64,
        }
    }
}

#[derive(Debug)]
pub struct Location {
    pub start_position: Position,
    pub end_position: Position,
}

impl From<Location> for ProtoLocation {
    fn from(val: Location) -> Self {
        ProtoLocation {
            start_position: Some(val.start_position.into()),
            end_position: Some(val.end_position.into()),
        }
    }
}
