use anyhow::anyhow;
use anyhow::Error;
use std::ops::DerefMut;
use std::sync::Arc;
use tracing::debug;

use crate::c_sharp_graph::query::Querier;
use crate::c_sharp_graph::query::Query;
use crate::c_sharp_graph::results::ResultNode;
use crate::provider::Project;

pub struct FindNode {
    #[allow(dead_code)]
    pub node_type: Option<String>,
    pub regex: String,
}

impl FindNode {
    pub fn run(self, project: &Arc<Project>) -> Result<Vec<ResultNode>, Error> {
        debug!("running search");

        let mut graph_guard = project.graph.lock().expect("unable to get project graph");
        let graph = match graph_guard.deref_mut() {
            Some(x) => x,
            None => {
                return Err(anyhow!("project graph not found, may not be initialized"));
            }
        };
        let source_node_type_info = match project.get_source_type() {
            Some(x) => x,

            None => {
                return Err(anyhow!(
                    "unable to get source node type, may not be initialized"
                ));
            }
        };
        let mut q = Querier::get_query(graph, Arc::as_ref(&source_node_type_info));

        q.query(self.regex)
    }
}
