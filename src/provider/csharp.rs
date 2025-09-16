use crate::c_sharp_graph::find_node::FindNode;
use crate::provider::AnalysisMode;
use crate::{
    analyzer_service::{
        provider_service_server::ProviderService, CapabilitiesResponse, Capability, Config,
        DependencyDagResponse, DependencyResponse, EvaluateRequest, EvaluateResponse,
        IncidentContext, InitResponse, NotifyFileChangesRequest, NotifyFileChangesResponse,
        ProviderEvaluateResponse, ServiceRequest,
    },
    provider::Project,
};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tonic::{Request, Response, Status};
use tracing::field::debug;
use tracing::{debug, error, info};
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
        let tools = Project::get_tools(&saved_config.provider_specific_config)
            .map_err(|e| Status::invalid_argument(format!("unalble to find tools: {}", e)))?;
        let project = Arc::new(Project::new(
            location,
            self.db_path.clone(),
            analysis_mode,
            tools,
        ));
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

        info!(
            "starting to load project for location: {:?}",
            project.location
        );
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
        info!("adding depdencies to stack graph database");
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
            node_type: condition.referenced.location.clone(),
            regex: condition.referenced.pattern.clone(),
        };

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
                info!("found {} results for search: {:?}", res.len(), &condition);
                let mut i: Vec<IncidentContext> = res.into_iter().map(Into::into).collect();
                i.sort_by_key(|i| format!("{}-{:?}", i.file_uri, i.line_number()));
                EvaluateResponse {
                    error: String::new(),
                    successful: true,
                    response: Some(ProviderEvaluateResponse {
                        matched: !i.is_empty(),
                        incident_contexts: i,
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
