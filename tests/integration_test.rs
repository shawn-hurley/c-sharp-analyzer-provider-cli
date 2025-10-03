use std::path::absolute;
use std::{fs::File, path::PathBuf, str::FromStr};

use prost_types::value::Kind::StringValue;
use prost_types::Value;
use serde::Deserialize;
use walkdir::WalkDir;

use c_sharp_analyzer_provider_cli::analyzer_service::IncidentContext;
use c_sharp_analyzer_provider_cli::analyzer_service::{
    provider_service_client::ProviderServiceClient, EvaluateRequest,
};
use c_sharp_analyzer_provider_cli::c_sharp_graph::results::ResultNode;

#[derive(Deserialize, Debug)]
pub struct TestEvaluateRequest {
    id: i64,
    cap: String,
    condition_info: String,
}

impl From<TestEvaluateRequest> for EvaluateRequest {
    fn from(value: TestEvaluateRequest) -> Self {
        EvaluateRequest {
            id: value.id,
            cap: value.cap,
            condition_info: value.condition_info,
        }
    }
}

#[tokio::test]
async fn integration_tests() {
    let mut client = ProviderServiceClient::connect("http://localhost:9000")
        .await
        .unwrap();
    let current_file = file!();
    let file_path = absolute(PathBuf::from_str(current_file).unwrap()).unwrap();
    println!("{:?}", file_path);

    let parent = file_path.parent().unwrap();
    let base = parent.parent().unwrap();
    let base: String = base.to_string_lossy().into_owned();
    let demos_path = parent.to_path_buf().join("demos");
    println!("Walking dir: {:?}", demos_path);
    for entry in WalkDir::new(&demos_path) {
        let entry = entry.unwrap();
        if !entry.file_type().is_dir() {
            continue;
        }
        let request_file = entry.clone().into_path().join("request.yaml");
        if !request_file.exists() {
            continue;
        }
        let demo_ouput = entry.clone().into_path().join("demo-output.yaml");
        if !demo_ouput.exists() {
            continue;
        }

        println!("Testing: {:?}", entry.path());
        let requst_file = File::open(&request_file).unwrap();

        let request: TestEvaluateRequest = serde_yml::from_reader(requst_file).unwrap();
        let request: EvaluateRequest = request.into();

        let result = client.evaluate(request).await.unwrap().into_inner();
        println!("{:?}", result);
        assert!(result.successful);
        let expected_file = File::open(&demo_ouput).unwrap();
        let expected_output: Vec<ResultNode> = serde_json::from_reader(expected_file).unwrap();
        let expected_output: Vec<IncidentContext> = expected_output
            .iter()
            .map(|rn| {
                let mut x: IncidentContext = (*rn).clone().into();
                if x.file_uri.contains("<REPLACE_ME>") {
                    x.file_uri = x.file_uri.replace("<REPLACE_ME>", &base);
                    let mut var = x.variables.clone().unwrap();
                    if let Some(s) = var.fields.get("file") {
                        if let Some(StringValue(y)) = &s.kind {
                            var.fields.insert(
                                "file".to_string(),
                                Value {
                                    kind: Some(StringValue(y.replace("<REPLACE_ME>", &base))),
                                },
                            );
                        }
                    }
                    x.variables = Some(var);
                }

                x
            })
            .collect();
        match result.response {
            None => panic!(),
            Some(x) => {
                assert_eq!(x.incident_contexts.len(), expected_output.len());
                for (i, ic) in x.incident_contexts.iter().enumerate() {
                    assert_eq!(
                        ic,
                        expected_output.get(i).unwrap(),
                        "test case: {:?}",
                        entry
                    );
                }
            }
        }
    }
}
