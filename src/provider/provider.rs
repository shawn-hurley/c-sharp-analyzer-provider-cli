use anyhow::Error;
use std::sync::Arc;
use std::{path::PathBuf, process::Command};

use crate::{
    analyzer_service::{
        provider_service_server::ProviderService, CapabilitiesResponse, Capability, Config,
        DependencyDagResponse, DependencyResponse, EvaluateRequest, EvaluateResponse,
        IncidentContext, InitResponse, Location, NotifyFileChangesRequest,
        NotifyFileChangesResponse, Position, ProviderEvaluateResponse, ServiceRequest,
    },
    provider::dependency_resolution::get_project_dependencies,
};
use prost_types::Struct;
use tokio::task::JoinHandle;
use tokio::{sync::Mutex, task};
use tonic::{Request, Response, Status};
use utoipa::{OpenApi, ToSchema};

use crate::c_sharp_graph::{find_node::FindNode, loader::load_database};
use crate::provider::{Dependencies, ProjectDependencies};
use serde::Deserialize;

#[derive(ToSchema, Deserialize, Debug)]
struct ReferenceCondition {
    pattern: String,
    location: Option<String>,
    #[allow(dead_code)]
    file_paths: Option<Vec<String>>,
}

#[derive(ToSchema, Deserialize, Debug)]
struct CSharpCondition {
    referenced: ReferenceCondition,
}

pub struct CSharpProvider {
    pub db_path: PathBuf,
    pub config: Mutex<Option<Arc<Config>>>,
}

#[tonic::async_trait]
impl ProviderService for CSharpProvider {
    async fn capabilities(&self, _: Request<()>) -> Result<Response<CapabilitiesResponse>, Status> {
        // Add Referenced

        #[derive(OpenApi)]
        struct ApiDoc;

        let openapi = ApiDoc::openapi();
        let json = openapi.to_pretty_json();
        if json.is_err() {
            return Err(Status::from_error(Box::new(json.err().unwrap())));
        }

        println!("returning refernced capability: {:?}", json.ok());

        return Ok(Response::new(CapabilitiesResponse {
            capabilities: vec![Capability {
                name: "referenced".to_string(),
                template_context: None,
            }],
        }));
    }

    async fn init(&self, r: Request<Config>) -> Result<Response<InitResponse>, Status> {
        let config = Arc::new(r.get_ref().clone());
        if self.config.lock().await.is_some() {
            return Err(Status::already_exists("already initialized"));
        }
        // Get the location from the config before moving the reference to self.
        let mut m = self.config.lock().await;
        let saved_config = m.insert(config);
        let _ = m;
        let x = saved_config.location.clone();

        let get_deps_handle: JoinHandle<Result<Vec<Dependencies>, Error>> = task::spawn(async {
            let resolver = get_project_dependencies(x);

            return resolver.resolve().await;
        });

        println!("db_path {:?}", self.db_path);
        let path = PathBuf::from(saved_config.location.clone());
        let stats = load_database(path, self.db_path.to_path_buf());
        println!("loaded files: {:?}", stats);

        let res = match get_deps_handle.await {
            Ok(res) => match res {
                Ok(res) => res,
                Err(e) => {
                    println!("unable to get deps: {}", e);
                    Vec::new()
                }
            },
            Err(e) => {
                println!("unable to get deps: {}", e);
                Vec::new()
            }
        };
        println!("got task result: {:?}", res);

        return Ok(Response::new(InitResponse {
            error: "".to_string(),
            successful: true,
            id: 4,
            builtin_config: None,
        }));
    }

    async fn evaluate(
        &self,
        r: Request<EvaluateRequest>,
    ) -> Result<Response<EvaluateResponse>, Status> {
        println!("request: {:?}", r);
        let evaluate_request = r.get_ref();
        println!("evaluate request: {:?}", evaluate_request.condition_info);

        if evaluate_request.cap != "referenced" {
            return Err(Status::invalid_argument("unknown capabilitys"));
        }
        let condition: CSharpCondition =
            match serde_yml::from_str(evaluate_request.condition_info.as_str()) {
                Ok(condition) => condition,
                Err(err) => {
                    println!("{:?}", err);
                    return Err(Status::new(tonic::Code::Internal, "failed"));
                }
            };
        println!("condition: {:?}", condition);
        let search = FindNode {
            node_type: condition.referenced.location,
            regex: condition.referenced.pattern,
        };
        let results = match search.run(&self.db_path) {
            Ok(res) => {
                // TODO convert Vec<Result> to ProviderEvaluateResponse
                if res.is_empty() {
                    EvaluateResponse {
                        error: "".to_string(),
                        successful: true,
                        response: Some(ProviderEvaluateResponse {
                            matched: false,
                            incident_contexts: vec![],
                            template_context: None,
                        }),
                    }
                } else {
                    EvaluateResponse {
                        error: "".to_string(),
                        successful: true,
                        response: Some(ProviderEvaluateResponse {
                            matched: true,
                            incident_contexts: res
                                .iter()
                                .map(|r| IncidentContext {
                                    file_uri: r.file_uri.clone(),
                                    effort: None,
                                    code_location: Some(Location {
                                        start_position: Some(Position {
                                            line: r.code_location.start_position.line as f64,
                                            character: r.code_location.start_position.character
                                                as f64,
                                        }),
                                        end_position: Some(Position {
                                            line: r.code_location.end_position.line as f64,
                                            character: r.code_location.end_position.character
                                                as f64,
                                        }),
                                    }),
                                    line_number: Some(r.line_number as i64),
                                    variables: Some(Struct {
                                        fields: r.variables.clone(),
                                    }),
                                    links: vec![],
                                    is_dependency_incident: false,
                                })
                                .collect(),
                            template_context: None,
                        }),
                    }
                }
            }
            Err(err) => {
                // TODO convert to EvaluateResponse for error
                EvaluateResponse {
                    error: err.to_string(),
                    successful: false,
                    response: None,
                }
            }
        };

        return Ok(Response::new(results));
    }
    async fn stop(&self, _: Request<ServiceRequest>) -> Result<Response<()>, Status> {
        return Ok(Response::new(()));
    }

    async fn get_dependencies(
        &self,
        _: Request<ServiceRequest>,
    ) -> Result<Response<DependencyResponse>, Status> {
        return Ok(Response::new(DependencyResponse {
            successful: true,
            error: "".to_string(),
            file_dep: vec![],
        }));
    }

    async fn get_dependencies_dag(
        &self,
        _: Request<ServiceRequest>,
    ) -> Result<Response<DependencyDagResponse>, Status> {
        return Ok(Response::new(DependencyDagResponse {
            successful: true,
            error: "".to_string(),
            file_dag_dep: vec![],
        }));
    }

    async fn notify_file_changes(
        &self,
        _: Request<NotifyFileChangesRequest>,
    ) -> Result<Response<NotifyFileChangesResponse>, Status> {
        return Ok(Response::new(NotifyFileChangesResponse {
            error: "".to_string(),
        }));
    }
}
