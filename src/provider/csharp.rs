use crate::c_sharp_graph::find_node::FindNode;
use crate::c_sharp_graph::results::ResultNode;
use crate::provider::AnalysisMode;
use crate::{
    analyzer_service::{
        provider_service_server::ProviderService, CapabilitiesResponse, Capability, Config,
        DependencyDagResponse, DependencyResponse, EvaluateRequest, EvaluateResponse,
        IncidentContext, InitResponse, Location, NotifyFileChangesRequest,
        NotifyFileChangesResponse, Position, ProviderEvaluateResponse, ServiceRequest,
    },
    provider::Project,
};
use prost_types::Struct;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tonic::{Request, Response, Status};
use tracing::{debug, error};
use utoipa::{OpenApi, ToSchema};

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
    pub config: Arc<Mutex<Option<Config>>>,
    pub project: Arc<Mutex<Option<Arc<Project>>>>,
}

impl CSharpProvider {
    pub fn new(db_path: PathBuf) -> CSharpProvider {
        CSharpProvider {
            db_path,
            config: Arc::new(Mutex::new(None)),
            project: Arc::new(Mutex::new(None)),
        }
    }
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

        debug!("returning refernced capability: {:?}", json.ok());

        return Ok(Response::new(CapabilitiesResponse {
            capabilities: vec![Capability {
                name: "referenced".to_string(),
                template_context: None,
            }],
        }));
    }

    async fn init(&self, r: Request<Config>) -> Result<Response<InitResponse>, Status> {
        let mut config_guard = self.config.lock().await;
        let saved_config = config_guard.insert(r.get_ref().clone());

        let analysis_mode = AnalysisMode::from(saved_config.analysis_mode.clone());
        let location = PathBuf::from(saved_config.location.clone());
        let project = Arc::new(Project::new(location, self.db_path.clone(), analysis_mode));
        let project_lock = self.project.clone();
        let mut project_guard = project_lock.lock().await;
        let _ = project_guard.replace(project.clone());
        drop(project_guard);
        drop(config_guard);

        let project_guard = project_lock.lock().await;
        let project = match project_guard.as_ref() {
            Some(x) => x,
            None => {
                return Err(Status::internal(
                    "unable to create language configuration for project",
                ));
            }
        };

        if let Err(e) = project.validate_language_configuration().await {
            error!("unable to create language configuration: {}", e);
            return Err(Status::internal(
                "unable to create language configuration for project",
            ));
        }
        let stats = project.get_project_graph().await.map_err(|err| {
            error!("{:?}", err);
            Status::new(tonic::Code::Internal, "failed")
        })?;
        debug!("loaded files: {:?}", stats);
        let get_deps_handle = project.resolve();

        let res = match get_deps_handle.await {
            Ok(res) => res,
            Err(e) => {
                debug!("unable to get deps: {}", e);
                return Err(Status::internal("unable to resolve dependenies"));
            }
        };
        debug!("got task result: {:?} -- project: {:?}", res, project);
        let res = project.load_to_database().await;
        debug!(
            "loading project to database: {:?} -- project: {:?}",
            res, project
        );

        return Ok(Response::new(InitResponse {
            error: String::new(),
            successful: true,
            id: 4,
            builtin_config: None,
        }));
    }

    async fn evaluate(
        &self,
        r: Request<EvaluateRequest>,
    ) -> Result<Response<EvaluateResponse>, Status> {
        debug!("request: {:?}", r);
        let evaluate_request = r.get_ref();
        debug!("evaluate request: {:?}", evaluate_request.condition_info);

        if evaluate_request.cap != "referenced" {
            return Err(Status::invalid_argument("unknown capabilities"));
        }
        let condition: CSharpCondition =
            serde_yml::from_str(evaluate_request.condition_info.as_str()).map_err(|err| {
                error!("{:?}", err);
                Status::new(tonic::Code::Internal, "failed")
            })?;

        debug!("condition: {:?}", condition);
        let search = FindNode {
            node_type: condition.referenced.location,
            regex: condition.referenced.pattern,
        };

        fn to_incident(r: &ResultNode) -> IncidentContext {
            IncidentContext {
                file_uri: r.file_uri.clone(),
                effort: None,
                code_location: Some(Location {
                    start_position: Some(Position {
                        line: r.code_location.start_position.line as f64,
                        character: r.code_location.start_position.character as f64,
                    }),
                    end_position: Some(Position {
                        line: r.code_location.end_position.line as f64,
                        character: r.code_location.end_position.character as f64,
                    }),
                }),
                line_number: Some(r.line_number as i64),
                variables: Some(Struct {
                    fields: r.variables.clone(),
                }),
                links: vec![],
                is_dependency_incident: false,
            }
        }

        let project_guard = self.project.lock().await;
        let project = match project_guard.as_ref() {
            Some(x) => x,
            None => {
                return Err(Status::internal("project may not be initialized"));
            }
        };
        let results = search.run(project).await.map_or_else(
            |err| EvaluateResponse {
                error: err.to_string(),
                successful: false,
                response: None,
            },
            |res| {
                // TODO convert Vec<Result> to ProviderEvaluateResponse
                EvaluateResponse {
                    error: String::new(),
                    successful: true,
                    response: Some(ProviderEvaluateResponse {
                        matched: !res.is_empty(),
                        incident_contexts: res.iter().map(to_incident).collect(),
                        template_context: None,
                    }),
                }
            },
        );

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
            error: String::new(),
            file_dep: vec![],
        }));
    }

    async fn get_dependencies_dag(
        &self,
        _: Request<ServiceRequest>,
    ) -> Result<Response<DependencyDagResponse>, Status> {
        return Ok(Response::new(DependencyDagResponse {
            successful: true,
            error: String::new(),
            file_dag_dep: vec![],
        }));
    }

    async fn notify_file_changes(
        &self,
        _: Request<NotifyFileChangesRequest>,
    ) -> Result<Response<NotifyFileChangesResponse>, Status> {
        return Ok(Response::new(NotifyFileChangesResponse {
            error: String::new(),
        }));
    }
}
