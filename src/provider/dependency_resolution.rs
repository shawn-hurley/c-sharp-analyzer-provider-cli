use anyhow::{anyhow, Error};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::Mutex;
use tokio::fs::{self, File};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::task::JoinSet;
use tracing::{debug, error, info};

use crate::c_sharp_graph::loader::load_database;

const INTERNAL_CLASS_MODULE_STRING: &str = "internal class <Module>";
const IMPLICT_BASE_CONSTRUCTOR_CALL: &str = "..ctor(";
const REFERNCE_ASSEMBLIES_NAME: &str = "Microsoft.NETFramework.ReferenceAssemblies";
const COMPILER_GENERATED_ANNOTATION: &str = "[CompilerGenerated]";

#[derive(Debug)]
pub struct Dependencies {
    pub location: PathBuf,
    #[allow(dead_code)]
    pub name: String,
    #[allow(dead_code)]
    pub version: String,
    pub decompiled_location: Arc<Mutex<HashSet<PathBuf>>>,
}

#[derive(Debug)]
pub struct Project {
    pub location: String,
    pub dependencies: Arc<Mutex<Option<Vec<Dependencies>>>>,
}

impl Project {
    pub fn new(location: String) -> Arc<Project> {
        Arc::new(Project {
            location,
            dependencies: Arc::new(Mutex::new(None)),
        })
    }

    pub async fn resolve(self: &Arc<Self>) -> Result<(), Error> {
        // First need to run packet.
        // Need to convert and download all DLL's
        //TODO: Add paket location as a provider specific config.
        let paket_output = Command::new("/Users/shurley/.dotnet/tools/paket")
            .args(["convert-from-nuget", "-f"])
            .current_dir(self.location.as_str())
            .output()?;

        let deps_response = self.read_packet_output(paket_output);
        let mut join_set = match deps_response {
            Ok(d) => d,
            Err(e) => {
                return Err(e);
            }
        };
        let mut deps: Vec<Dependencies> = Vec::new();
        while let Some(res) = join_set.join_next().await {
            match res {
                Ok(d) => {
                    deps.push(d);
                }
                Err(e) => {
                    return Err(Error::new(e));
                }
            }
        }
        let mut d = self.dependencies.lock().unwrap();
        *d = Some(deps);

        Ok(())
    }

    fn read_packet_output(&self, output: Output) -> Result<JoinSet<Dependencies>, Error> {
        if !output.status.success() {
            //TODO: Consider a specific error type
            debug!("paket command not successful");
            return Err(Error::msg("paket command did not succeed"));
        }

        // We need to get the Reference Assemblies after we successfully
        // convert to paket.
        // Either this will be input into the init
        // Or we will find a clever way to get it from the .csproj file
        // For speed going to hardcoded for now.
        let paket_output = Command::new("/Users/shurley/.dotnet/tools/paket")
            .args([
                "add",
                format!("{}.net45", REFERNCE_ASSEMBLIES_NAME).as_str(),
            ])
            .current_dir(self.location.as_str())
            .output()?;
        debug!("{:?}", paket_output);
        if !paket_output.status.success() {
            debug!("unable to add reference assemblies");
            return Err(anyhow!("unable to add reference Assemblies"));
        }
        let lines = String::from_utf8_lossy(&output.stdout).to_string();
        let path = PathBuf::from(&self.location);
        let reference_assmblies = Path::new(&self.location).join(
            "packages/Microsoft.NETFramework.ReferenceAssemblies.net45/build/.NETFramework/v4.5",
        );

        // Exampale lines to parse:
        // - Microsoft.SqlServer.Types is pinned to 10.50.1600.1
        // - Newtonsoft.Json is pinned to 5.0.4
        // - EntityFramework is pinned to 5.0.0
        // - DotNetOpenAuth.AspNet is pinned to 4.3.0.13117
        let mut set = JoinSet::new();
        for line in lines.lines() {
            if !line.contains("-") || !line.contains("is pinned to") {
                continue;
            }

            let parts: Vec<&str> = line.split("is pinned to").collect();

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

            let d = Dependencies {
                location: dep_path,
                name: name.to_string(),
                version: version.to_string(),
                decompiled_location: Arc::new(Mutex::new(HashSet::new())),
            };
            let reference_assmblies = reference_assmblies.clone();
            set.spawn(async move {
                info!("DECOMPILE!!!");
                let decomp = d.decompile(reference_assmblies).await;
                if let Err(e) = decomp {
                    error!("could not decompile - {:?}", e);
                }
                d
            });
        }

        Ok(set)
    }

    pub async fn load_to_database(&self, db_path: PathBuf) -> Result<(), Error> {
        let db = Arc::new(db_path);
        let shared_deps = Arc::clone(&self.dependencies);
        let mut x = shared_deps.lock().unwrap();
        if let Some(ref mut vec) = *x {
            //do something
            for d in vec {
                let decompiled_locations: Arc<Mutex<HashSet<PathBuf>>> =
                    Arc::clone(&d.decompiled_location);
                let decompiled_locations = decompiled_locations.lock().unwrap();
                let decompiled_files = &(*decompiled_locations);
                for decompiled_file in decompiled_files {
                    debug!("loading file {:?} into database", &decompiled_file);
                    let stats = load_database(decompiled_file, db.to_path_buf());
                    debug!("loaded file: {:?} stats: {:?}", &decompiled_file, stats);
                }
            }
        }
        Ok(())
    }
}

impl Dependencies {
    pub async fn decompile(&self, reference_assmblies: PathBuf) -> Result<(), Error> {
        // TODO: make location of ilspycmd decompilation
        let dep_package_dir = self.location.to_owned();
        if !dep_package_dir.is_dir() || !dep_package_dir.exists() {
            return Err(anyhow!("invalid package path: {:?}", dep_package_dir));
        }
        let mut entries = fs::read_dir(dep_package_dir).await?;
        let mut paket_cache_file: Option<PathBuf> = None;
        while let Some(entry) = entries.next_entry().await? {
            // Find the paket_installmodel.cache file to read
            // and find the .dll's
            if entry.file_name().to_string_lossy() == "paket-installmodel.cache" {
                paket_cache_file = Some(entry.path());
                break;
            }
        }
        let to_decompile_locations = match paket_cache_file {
            Some(cache_file) => {
                // read_cache_file to get the path to the last found dll
                // this is an aproximation of what we want and eventually
                // we will need to understand the packet.dependencies file
                self.read_packet_cache_file(cache_file).await?
            }
            None => {
                debug!("did not find a dll for dep: {:?}", self);
                return Ok(());
            }
        };
        let mut decompiled_files: HashSet<PathBuf> = HashSet::new();
        for file_to_decompile in to_decompile_locations {
            let decompiled_file = self
                .decompile_file(&reference_assmblies, file_to_decompile)
                .await?;
            decompiled_files.insert(decompiled_file);
        }

        let mut guard = self.decompiled_location.lock().unwrap();
        *guard = decompiled_files;
        drop(guard);

        Ok(())
    }

    async fn read_packet_cache_file(&self, file: PathBuf) -> Result<Vec<PathBuf>, Error> {
        info!("Reading packet cache file: {:?}", file);
        let file = File::open(file).await;
        if let Err(e) = file {
            error!("unable to find error: {:?}", e);
            return Err(anyhow!(e));
        }
        let reader = BufReader::new(file.ok().unwrap());
        let mut lines = reader.lines();
        let mut dlls: Vec<String> = vec![];
        let top_of_version = "D: /lib/net45".to_string();
        let mut valid_dir_to_search = "".to_string();
        let mut valid_file_match_start = "".to_string();

        while let Some(line) = lines.next_line().await? {
            if line.contains("D: /lib/")
                && line <= top_of_version
                && (valid_file_match_start.is_empty() || line > valid_dir_to_search)
            {
                valid_file_match_start = line.replace("D:", "F:");
                valid_dir_to_search = line.clone();
                dlls = vec![];
            }
            if line.contains(".dll")
                && !valid_dir_to_search.is_empty()
                && line.starts_with(&valid_file_match_start)
            {
                dlls.push(line);
            }
        }
        let dll_paths: Vec<PathBuf> = dlls
            .iter()
            .map(|x| {
                let p = self.location.join(x.trim_start_matches("F: /"));
                if !p.exists() {
                    debug!("unable to find path: {:?}", p);
                }
                p
            })
            .collect();

        if dlls.is_empty() {
            error!("Unable to get dlls from file");
        }
        Ok(dll_paths)
    }

    async fn decompile_file(
        &self,
        reference_assmblies: &PathBuf,
        file_to_decompile: PathBuf,
    ) -> Result<PathBuf, Error> {
        let decompile_name = match self.location.as_path().file_name() {
            Some(n) => {
                let mut x = n.to_owned().to_string_lossy().into_owned();
                x.push_str("-decompiled");
                x
            }
            None => return Err(anyhow!("unable to dependency name")),
        };
        let decompile_out_name = match file_to_decompile.parent() {
            Some(p) => p.join(decompile_name),
            None => {
                return Err(anyhow!("unable to get path"));
            }
        };
        let decompile_output = Command::new("/Users/shurley/.dotnet/tools/ilspycmd")
            .arg("-o")
            .arg(&decompile_out_name)
            .arg("-r")
            .arg(reference_assmblies)
            .arg("--no-dead-code")
            .arg("--no-dead-stores")
            .arg("-lv")
            .arg("CSharp7_3")
            .arg("-p")
            .arg(&file_to_decompile)
            .current_dir(&self.location)
            .output()?;

        debug!("decompile output: {:?}", decompile_output);

        let decompiled_file_name = match file_to_decompile.file_stem() {
            Some(s) => s.to_string_lossy(),
            None => {
                return Err(anyhow!("unable to get file stem for dll"));
            }
        };

        // read the file that was decompiled, and look for invalid
        // things that are not valid C# but come from the intermediate language
        //let decompile_out_name =
        //   decompile_out_name.join(format!("{}.decompiled.cs", decompiled_file_name));

        /*
        let file = match File::open(&decompile_out_name).await {
            Ok(f) => f,
            Err(e) => {
                return Err(anyhow!(
                    "unable to open file: {:?} - {}",
                    &decompile_out_name,
                    e
                ));
            }
        };

        let reader = BufReader::new(file);
        let mut lines = reader.lines();

        let mut in_class_module = false;
        let mut in_compiler_generated = false;
        let mut bracket_matching = 0;
        let mut compiler_generated_classes: Vec<String> = vec![];
        let mut new_lines: Vec<String> = vec![];
        let mut old_lines: Vec<String> = vec![];
        let mut compler_generated_line: Option<String> = None;
        while let Some(line) = lines.next_line().await? {
            old_lines.push(line.clone());
            if line.contains(INTERNAL_CLASS_MODULE_STRING) {
                in_class_module = true;
                continue;
            }
            // These sections allow me to determine when the closing brack for
            // the internal Module is for the last one.
            if in_class_module && line.contains("{") {
                bracket_matching += 1;
                continue;
            }
            if in_class_module && line.contains("}") {
                bracket_matching -= 1;
                if bracket_matching == 0 {
                    in_class_module = false;
                }
                continue;
            }
            if in_class_module {
                continue;
            }
            // Handle when the decompiler can't determine the type for LHS
            //
            if line.contains("? ") {
                let trimmed_line = line.as_str();
                //
                if trimmed_line.trim().starts_with("? ") {
                    let line = trimmed_line.replacen("? ", "System.Object", 1);
                    new_lines.push(line);
                    continue;
                }
            }
            if line.contains(IMPLICT_BASE_CONSTRUCTOR_CALL) {
                continue;
            }
            if line.contains(COMPILER_GENERATED_ANNOTATION) {
                compler_generated_line = Some(line);
                continue;
            }
            if let Some(prev_line) = &compler_generated_line {
                // These appear to be notations that ILSPY uses to denote
                // that it can not resolve.
                if line.contains("ctor>") || line.contains("<>") {
                    // Strip out the class name if one,
                    // add to list of classes to ignore.
                    let mut parts = line.split("class ");
                    if let Some(class_name_part) = parts.nth(1) {
                        compiler_generated_classes
                            .push(class_name_part.to_string().trim().to_string());
                    }
                    in_compiler_generated = true;
                    compler_generated_line = None;
                    continue;
                }
                new_lines.push(prev_line.clone());
                compler_generated_line = None;
            }
            if in_compiler_generated && line.contains("{") {
                bracket_matching += 1;
                continue;
            }
            if in_compiler_generated && line.contains("}") {
                bracket_matching -= 1;
                if bracket_matching == 0 {
                    info!("exit in compiler generated module");
                    in_compiler_generated = false;
                }
                continue;
            }
            if in_compiler_generated {
                continue;
            }

            // Ignore preproccessor additions from ilspy.
            if line.contains("#define") {
                continue;
            }

            // If there is a compiler generated class and the line references we should skip that
            // line.
            if compiler_generated_classes.iter().any(|f| line.contains(f)) {
                debug!("Here skip");
                continue;
            }
            new_lines.push(line);
        }
        drop(lines);

        if new_lines.is_empty() {
            error!(
                "NO LINES WRITEN: {:?} -> falling back to old lines",
                &decompile_out_name
            );
            new_lines = old_lines;
        }

        let lines = new_lines.join("\n");

        let res = tokio::fs::write(&decompile_out_name, lines).await;
        */

        Ok(decompile_out_name)
    }
}
