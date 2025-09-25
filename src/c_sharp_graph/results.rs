use std::{collections::BTreeMap, fmt::Display, str::FromStr};

use prost_types::{Struct, Value};
use serde::{Deserialize, Deserializer};
use serde_json::json;

use crate::analyzer_service::{
    IncidentContext, Location as ProtoLocation, Position as ProtoPosition,
};

#[derive(Debug, Deserialize, Clone)]
pub struct ResultNode {
    #[serde(rename = "fileURI")]
    pub file_uri: String,
    #[serde(rename = "LineNumber", deserialize_with = "string_to_usize")]
    pub line_number: usize,
    pub variables: BTreeMap<std::string::String, serde_json::Value>,
    #[serde(rename = "codeLocation")]
    pub code_location: Location,
}

fn string_to_usize<'de, T, D>(deserializer: D) -> Result<T, D::Error>
where
    T: Deserialize<'de> + FromStr,
    D: Deserializer<'de>,
    <T as FromStr>::Err: Display,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrNumber<T> {
        String(String),
        Number(T),
    }

    match StringOrNumber::<T>::deserialize(deserializer)? {
        StringOrNumber::String(s) => s.parse::<T>().map_err(serde::de::Error::custom),
        StringOrNumber::Number(i) => Ok(i),
    }
}

fn serde_json_to_prost(json: serde_json::Value) -> prost_types::Value {
    use prost_types::value::Kind::*;
    use serde_json::Value::*;
    prost_types::Value {
        kind: Some(match json {
            Null => NullValue(0 /* wat? */),
            Bool(v) => BoolValue(v),
            Number(n) => NumberValue(n.as_f64().expect("Non-f64-representable number")),
            String(s) => StringValue(s),
            Array(v) => ListValue(prost_types::ListValue {
                values: v.into_iter().map(serde_json_to_prost).collect(),
            }),
            Object(v) => {
                let mut new_map: BTreeMap<std::string::String, Value> = BTreeMap::new();
                for (k, val) in v {
                    new_map.insert(k, serde_json_to_prost(val));
                }
                StructValue(Struct { fields: new_map })
            }
        }),
    }
}

impl From<ResultNode> for IncidentContext {
    fn from(val: ResultNode) -> Self {
        let x = serde_json_to_prost(json!(val.variables.clone()));
        if let Some(prost_types::value::Kind::StructValue(x)) = x.kind {
            IncidentContext {
                file_uri: val.file_uri.clone(),
                effort: None,
                code_location: Some(val.code_location.into()),
                line_number: Some(val.line_number as i64),
                variables: Some(x),
                links: vec![],
                is_dependency_incident: false,
            }
        } else {
            IncidentContext {
                file_uri: val.file_uri.clone(),
                effort: None,
                code_location: Some(val.code_location.into()),
                line_number: Some(val.line_number as i64),
                variables: None,
                links: vec![],
                is_dependency_incident: false,
            }
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct Position {
    pub line: usize,
    #[serde(default)]
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

#[derive(Debug, Deserialize, Clone)]
pub struct Location {
    #[serde(rename = "startPosition")]
    pub start_position: Position,
    #[serde(rename = "endPosition")]
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
