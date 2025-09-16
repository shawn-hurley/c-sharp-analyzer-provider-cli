use anyhow::{anyhow, Error};
use stack_graphs::graph::StackGraph;
use stack_graphs::partial::PartialPath;
use stack_graphs::partial::PartialPaths;
use stack_graphs::stitching::ForwardPartialPathStitcher;
use stack_graphs::stitching::StitcherConfig;
use stack_graphs::storage::SQLiteReader;
use stack_graphs::storage::SQLiteWriter;
use stack_graphs::NoCancellation;
use std::collections::HashSet;
use std::fmt::Debug;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::fs::{self, File};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::task::JoinSet;
use tracing::{debug, error, info, trace};

use crate::c_sharp_graph::loader::add_dir_to_graph;
use crate::c_sharp_graph::loader::SourceType;
use crate::provider::project::Tools;
use crate::provider::Project;

const REFERNCE_ASSEMBLIES_NAME: &str = "Microsoft.NETFramework.ReferenceAssemblies";
pub struct Dependencies {
    pub location: PathBuf,
    #[allow(dead_code)]
    pub name: String,
    #[allow(dead_code)]
    pub version: String,
    pub decompiled_location: Arc<Mutex<HashSet<PathBuf>>>,
}

impl Debug for Dependencies {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("\nDependencies")
            .field("name", &self.name)
            .field("version", &self.version)
            .field("location", &self.location)
            .finish()
    }
}

impl Dependencies {
    pub async fn decompile(
        &self,
        reference_assmblies: PathBuf,
        restriction: String,
        tools: &Tools,
    ) -> Result<(), Error> {
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
                self.read_packet_cache_file(cache_file, restriction).await?
            }
            None => {
                debug!("did not find a dll for dep: {:?}", self);
                return Err(anyhow!("unable to find dll's"));
            }
        };
        let mut decompiled_files: HashSet<PathBuf> = HashSet::new();
        for file_to_decompile in to_decompile_locations {
            let decompiled_file = self
                .decompile_file(
                    &reference_assmblies,
                    file_to_decompile,
                    tools.ilspy_cmd.clone(),
                )
                .await?;
            decompiled_files.insert(decompiled_file);
        }

        let mut guard = self.decompiled_location.lock().unwrap();
        *guard = decompiled_files;
        drop(guard);

        Ok(())
    }

    async fn read_packet_cache_file(
        &self,
        file: PathBuf,
        restriction: String,
    ) -> Result<Vec<PathBuf>, Error> {
        info!("Reading packet cache file: {:?}", file);
        let file = File::open(file).await;
        if let Err(e) = file {
            error!("unable to find error: {:?}", e);
            return Err(anyhow!(e));
        }
        let reader = BufReader::new(file.ok().unwrap());
        let mut lines = reader.lines();
        let mut dlls: Vec<String> = vec![];
        let top_of_version = format!("D: /lib/{}", restriction);
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
        ilspycmd: PathBuf,
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
        let decompile_output = Command::new(ilspycmd)
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

        trace!("decompile output: {:?}", decompile_output);

        Ok(decompile_out_name)
    }
}

impl Project {
    pub async fn resolve(&self) -> Result<(), Error> {
        // determine if the paket.dependencies already exists, if it does then we don't need to
        // convert.
        let paket_deps_file = self.location.clone().join("paket.dependencies");

        if !paket_deps_file.exists() {
            // Fsourcoirst need to run packet.
            // Need to convert and download all DLL's
            //TODO: Add paket location as a provider specific config.
            let paket_output = Command::new(&self.tools.paket_cmd)
                .args(["convert-from-nuget", "-f"])
                .current_dir(&self.location)
                .output()?;
            if !paket_output.status.success() {
                //TODO: Consider a specific error type
                debug!("paket command not successful");
                return Err(Error::msg("paket command did not succeed"));
            }
        }

        let (reference_assembly_path, highest_restriction, deps) = self
            .read_packet_dependency_file(paket_deps_file.as_path())
            .await?;
        debug!(
            "got: {:?} -- {:?}",
            reference_assembly_path, highest_restriction
        );
        let mut set = JoinSet::new();
        for d in deps {
            let reference_assmblies = reference_assembly_path.clone();
            let restriction = highest_restriction.clone();
            let tools = self.tools.clone();
            set.spawn(async move {
                let decomp = d.decompile(reference_assmblies, restriction, &tools).await;
                if let Err(e) = decomp {
                    error!("could not decompile - {:?}", e);
                }
                d
            });
        }
        // reset deps, as all the deps should be moved into the threads.
        let mut deps = vec![];
        while let Some(res) = set.join_next().await {
            match res {
                Ok(d) => {
                    deps.push(d);
                }
                Err(e) => {
                    return Err(Error::new(e));
                }
            }
        }
        let mut d = self.dependencies.lock().await;
        *d = Some(deps);

        Ok(())
    }

    pub async fn load_to_database(&self) -> Result<(), Error> {
        let shared_deps = Arc::clone(&self.dependencies);
        let mut x = shared_deps.lock().await;
        let mut set = JoinSet::new();
        if let Some(ref mut vec) = *x {
            // For each dependnecy in the list we will try and load the decompiled files
            // Into the stack graph database.
            for d in vec {
                let decompiled_locations: Arc<Mutex<HashSet<PathBuf>>> =
                    Arc::clone(&d.decompiled_location);
                let decompiled_locations = decompiled_locations.lock().unwrap();
                let decompiled_files = &(*decompiled_locations);
                for decompiled_file in decompiled_files {
                    let file = decompiled_file.clone();
                    let lc = self.source_language_config.clone();
                    let db_path = self.db_path.clone();
                    let dep_name = d.name.clone();
                    set.spawn(async move {
                        let mut graph = StackGraph::new();
                        // We need to make sure that the symols for source type are the first
                        // symbols, so that they match what is in the builtins.
                        let (_, _) = SourceType::load_symbols_into_graph(&mut graph);
                        // remove mutability
                        let graph = graph;
                        let lc_guard = lc.read().await;
                        let lc = match lc_guard.as_ref() {
                            Some(x) => x,
                            None => {
                                return Err(anyhow!("unable to get source language config"));
                            }
                        };

                        let graph = add_dir_to_graph(
                            &file,
                            &lc.dependnecy_type_node_info,
                            &lc.language_config,
                            graph,
                        )?;
                        drop(lc_guard);
                        let mut db: SQLiteWriter = SQLiteWriter::open(db_path)?;
                        for (file_path, tag) in graph.file_to_tag.clone() {
                            let file_str = file_path.to_string_lossy();
                            let file_handle = graph
                                .stack_graph
                                .get_file(&file_str)
                                .ok_or(anyhow!("unable to get file"))?;
                            let mut partials = PartialPaths::new();
                            let mut paths: Vec<PartialPath> = vec![];
                            let stats =
                                ForwardPartialPathStitcher::find_minimal_partial_path_set_in_file(
                                    &graph.stack_graph,
                                    &mut partials,
                                    file_handle,
                                    StitcherConfig::default().with_collect_stats(true),
                                    &NoCancellation,
                                    |_, _, p| paths.push(p.clone()),
                                )?;
                            db.store_result_for_file(
                                &graph.stack_graph,
                                file_handle,
                                &tag,
                                &mut partials,
                                &paths,
                            )?;
                            trace!("stats for stitiching: {:?} - paths: {}", stats, paths.len(),);
                        }
                        debug!(
                            "stats for dependency: {:?}, files indexed {:?}",
                            dep_name, graph.files_loaded,
                        );
                        Ok(graph)
                    });
                }
            }
        }
        for res in set.join_all().await {
            let init_graph = match res {
                Ok(i) => i,
                Err(e) => {
                    return Err(anyhow!(
                        "unable to get graph, project may not have been initialized: {}",
                        e
                    ));
                }
            };
            debug!(
                "loaded {} files for dep: {:?}",
                init_graph.files_loaded, init_graph.file_to_tag
            );
        }

        let mut graph_guard = self
            .graph
            .lock()
            .expect("project may not have been initialized");
        let mut db_reader = SQLiteReader::open(&self.db_path)?;
        db_reader.load_graphs_for_file_or_directory(&self.location, &NoCancellation)?;
        let (read_graph, partials, databse) = db_reader.get();
        let read_graph = read_graph.to_serializable();
        let mut new_graph = StackGraph::new();
        read_graph.load_into(&mut new_graph)?;
        debug!(
            "adding {:?} files from other graph",
            databse.to_serializable(&new_graph, partials)
        );
        let _ = graph_guard.insert(new_graph);

        Ok(())
    }

    async fn read_packet_dependency_file(
        &self,
        paket_deps_file: &Path,
    ) -> Result<(PathBuf, String, Vec<Dependencies>), Error> {
        let file = File::open(paket_deps_file).await;
        if let Err(e) = file {
            error!("unable to find error: {:?}", e);
            return Err(anyhow!(e));
        }
        let reader = BufReader::new(file.ok().unwrap());
        let mut lines = reader.lines();
        let mut smallest_framework = "zzzzzzzzzzzzzzz".to_string();
        let mut deps: Vec<Dependencies> = vec![];
        while let Some(line) = lines.next_line().await? {
            if !line.contains("restriction") {
                continue;
            }
            let parts: Vec<&str> = line.split("restriction:").collect();
            if parts.len() != 2 {
                continue;
            }
            if let Some(dep_part) = parts.first() {
                let white_space_split: Vec<&str> = dep_part.split_whitespace().collect();
                if white_space_split.len() < 4 {
                    continue;
                }
                let mut dep_path = self.location.clone();
                dep_path.push("packages");
                let name = match white_space_split.get(1) {
                    Some(n) => n,
                    None => {
                        continue;
                    }
                };
                dep_path.push(name);
                let version = match white_space_split.get(2) {
                    Some(v) => v,
                    None => {
                        continue;
                    }
                };
                let dep = Dependencies {
                    location: dep_path,
                    name: name.to_string(),
                    version: version.to_string(),
                    decompiled_location: Arc::new(Mutex::new(HashSet::new())),
                };
                deps.push(dep);
            }

            if let Some(ref_name) = parts.get(1) {
                let n = ref_name.to_string();
                if let Some(framework) = n.split_whitespace().last() {
                    let framework_string = framework.to_string();
                    if framework_string < smallest_framework {
                        smallest_framework = framework_string;
                    }
                }
            }
        }
        drop(lines);

        // Now we we have the framework, we need to get the reference_assmblies
        let base_name = format!("{}.{}", REFERNCE_ASSEMBLIES_NAME, smallest_framework);
        let paket_reference_output = Command::new(&self.tools.paket_cmd)
            .args(["add", base_name.as_str()])
            .current_dir(&self.location)
            .output()?;

        debug!("paket_reference_output: {:?}", paket_reference_output);

        let paket_install = match paket_deps_file.parent() {
            Some(dir) => dir.to_path_buf().join("packages").join(base_name),
            None => {
                return Err(anyhow!(
                    "unable to find the paket install of reference assembly"
                ));
            }
        };
        // Read the paket_install to find the directory of the DLL's
        let file = File::open(paket_install.join("paket-installmodel.cache")).await;
        if let Err(e) = file {
            error!("unable to find error: {:?}", e);
            return Err(anyhow!(e));
        }
        let reader = BufReader::new(file.ok().unwrap());
        let mut lines = reader.lines();
        while let Some(line) = lines.next_line().await? {
            if line.contains("build/.NETFramework/") && line.contains("D: /") {
                let path_str = match line.strip_prefix("D: /") {
                    Some(x) => x,
                    None => {
                        return Err(anyhow!("unable to get reference assembly"));
                    }
                };
                debug!("path_str: {}", path_str);
                let path = paket_install.join(path_str);
                return Ok((paket_install.join(path), smallest_framework, deps));
            }
        }

        Err(anyhow!("unable to get reference assembly"))
    }
}
