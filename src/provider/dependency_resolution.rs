use anyhow::Error;
use stack_graphs::paths::Extend;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tonic::async_trait;

#[derive(Debug)]
pub struct Dependencies {
    location: PathBuf,
    name: String,
    version: String,
}

#[async_trait]
pub trait ProjectDependencies {
    async fn resolve(&self) -> Result<Vec<Dependencies>, Error>;
}

struct Project {
    location: String,
}

pub fn get_project_dependencies(project_location: String) -> impl ProjectDependencies {
    return Project {
        location: project_location,
    };
}

#[async_trait]
impl ProjectDependencies for Project {
    async fn resolve(&self) -> Result<Vec<Dependencies>, Error> {
        // First need to run packet.
        // Need to convert and download all DLL's
        //TODO: Add paket location as a provider specific config.
        let paket_output = Command::new("/Users/shurley/.dotnet/tools/paket")
            .args(["convert-from-nuget", "-f"])
            .current_dir(self.location.as_str())
            .output()?;

        return self.read_packet_output(paket_output);
    }
}

impl Project {
    fn read_packet_output(&self, output: Output) -> Result<Vec<Dependencies>, Error> {
        if !output.status.success() {
            //TODO: Consider a specific error type
            println!("paket command not successful");
            return Err(Error::msg("paket command did not succeed"));
        }

        let lines = String::from_utf8_lossy(&output.stdout).to_string();
        let path = PathBuf::from(self.location.clone());

        // Exampale lines to parse:
        // - Microsoft.SqlServer.Types is pinned to 10.50.1600.1
        // - Newtonsoft.Json is pinned to 5.0.4
        // - EntityFramework is pinned to 5.0.0
        // - DotNetOpenAuth.AspNet is pinned to 4.3.0.13117
        let mut deps: Vec<Dependencies> = Vec::new();
        for line in lines.lines() {
            if !line.contains("-") || !line.contains("is pinned to") {
                continue;
            }

            let parts: Vec<&str> = line.split("is pinned to").collect();

            //if parts.len() != 2 {
            //TODO: Should error
            //    continue;
            //}

            // Example parts
            // [\" - DotNetOpenAuth.OpenId.Core \", \" 4.3.0.13117\"]"
            let name = match parts[0].trim().strip_prefix("- ") {
                Some(n) => n,
                None => parts[0],
            };
            let version = parts[1].trim();
            let mut dep_path = path.clone().to_path_buf();
            dep_path.push("packages");
            dep_path.push(name);

            deps.push(Dependencies {
                location: dep_path,
                name: name.to_string(),
                version: version.to_string(),
            });
        }

        return Ok(deps);
    }
}
